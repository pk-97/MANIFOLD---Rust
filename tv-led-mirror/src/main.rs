//! tv-led-mirror — capture a macOS display and stream its edges to ArtNet LEDs.
//!
//! Reuses Manifold's `manifold-led` pipeline end-to-end: the captured frame is
//! handed to `LedOutputController::process_frame` as a `GpuTexture` backed by
//! the IOSurface that `CGDisplayStream` delivers, so the edge-extend compute
//! shader / DMX packing / ArtNet send path are byte-identical to the main app.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureUsage};
use manifold_led::{LedOutputController, LedSettings, StripAddressing};
use parking_lot::{Mutex, RwLock};

mod capture;
mod ffi;
mod gui;
mod hue;
mod reload;
mod shutdown;
mod slicer;

use slicer::{ColorGrade, Crop, Slicer};

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

    /// Saturation multiplier on the final color. 1.0 = no change, >1 boosts
    /// (compensates for the LEDs' diffuse look — try 1.3-1.6 for a punchy
    /// ambient feel), <1 desaturates. Mixes around BT.709 luma.
    #[arg(long, default_value_t = 1.0)]
    vibrance: f32,

    /// Output gamma. Default 2.2 matches SK9822's linear PWM to perceptual
    /// brightness — mid-tones get squashed so "50% grey" pixels stop blasting
    /// the LEDs at 50% PWM. Set to 1.0 for raw linear output (debug only).
    #[arg(long, default_value_t = 2.2)]
    gamma: f32,

    /// Saturation bias on the blur weights. 0 = pure binomial average (the
    /// math reason small bright-orange regions smear into "warm white"
    /// against dark backgrounds). Higher = saturated pixels punch through
    /// the average more. Try 4-10. Only meaningful when --blur-radius > 0.
    #[arg(long, default_value_t = 0.0)]
    saturation_bias: f32,

    /// Peak-luminance bias on the blur weights. Without this, increasing
    /// `--blur-radius` dilutes bright peaks into surrounding darkness
    /// (averaging is intrinsically peak-suppressing). Default 4.0 keeps
    /// peaks intact even at wide blur. Set 0 for pure binomial average.
    #[arg(long, default_value_t = 4.0)]
    peak_bias: f32,

    /// White-balance trim, RED channel. SK9822 strips skew cool (~7500K);
    /// to pull toward TV D65 (~6500K), leave R=1.0 and dial B down.
    /// Suggested SK9822 → D65 starting point: --wb-r 1.00 --wb-g 0.97 --wb-b 0.82.
    #[arg(long, default_value_t = 1.0)]
    wb_r: f32,

    /// White-balance trim, GREEN channel.
    #[arg(long, default_value_t = 1.0)]
    wb_g: f32,

    /// White-balance trim, BLUE channel.
    #[arg(long, default_value_t = 1.0)]
    wb_b: f32,

    /// Output luminance ceiling (0..1). Caps how bright the LEDs can ever
    /// go without dragging colors toward gray (RGB rescales to keep chroma).
    /// 1.0 = no cap. 0.5-0.7 is a comfortable nighttime ceiling for SK9822
    /// strips, which can be dazzling at full white.
    #[arg(long, default_value_t = 1.0)]
    max_luminance: f32,

    /// Per-channel black floor: any output value below this drops to 0.
    /// Eliminates the flickery sub-PWM region where SK9822 strips strobe
    /// instead of dimming cleanly. Try 0.015 (= 4/255).
    #[arg(long, default_value_t = 0.0)]
    black_floor: f32,

    /// Blur multiplier on the LED tile's coverage area. 1.0 = 5×5 binomial
    /// taps span exactly one LED tile (proper area integration via mipmap
    /// LOD). >1 = wider blur (tiles bleed into neighbors), <1 = tighter.
    #[arg(long, default_value_t = 1.0)]
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

    /// Enable HDR capture path: ScreenCaptureKit with 16-bit float pixel
    /// format and extendedLinearSRGB colorspace. Required to actually react
    /// to HDR content (Q80T ST.2084 etc.) — without this macOS tone-maps the
    /// captured display down to SDR before we ever see it. SDR sources work
    /// through this path too.
    #[arg(long)]
    hdr: bool,

    /// Peak linear luminance for HDR tone-mapping. Linear extendedSRGB can
    /// run well above 1.0 — values above this get rolled off via Reinhard.
    /// Lower = more highlight compression; higher = more dynamic range
    /// preserved at the cost of mid-tones. 4.0 ≈ 1000 nits if your display
    /// peaks there. Only used when --hdr is set.
    #[arg(long, default_value_t = 4.0)]
    hdr_peak: f32,

    /// Capture in extendedLinearDisplayP3 instead of extendedLinearSRGB so
    /// wide-gamut HDR colors (vivid greens/cyans/magentas outside sRGB
    /// primaries) survive into the slicer. The shader applies a P3→sRGB
    /// matrix before LED output. Most useful with --hdr on a P3-capable
    /// display (e.g. a Samsung Q-series in HDR mode).
    #[arg(long)]
    p3: bool,

    /// Temporal smoothing factor (0..1). 1.0 = no smoothing (current frame
    /// passes through). Smaller = more inertia (3-frame EMA at 0.3). Removes
    /// per-frame flicker on text/UI/scene cuts at the cost of a few frames
    /// of latency. Try 0.4 for movies, 0.7-1.0 for fast games.
    #[arg(long, default_value_t = 1.0)]
    smoothing: f32,

    // ─── Philips Hue Entertainment streaming ──────────────────────────────
    //
    // Streams one dominant+brightest color from the scene to a Hue
    // Entertainment Area (set up in the Hue app) at 25 Hz over DTLS-PSK.
    // The side LEDs do detail; the Hue bulbs do the wash.
    //
    // First-time setup: run `--hue-pair --hue-bridge <ip>`, follow the
    // prompts, paste the printed flags into flags.conf.
    /// One-shot: register this app on the bridge at `--hue-bridge <ip>`,
    /// print credentials, list available entertainment areas, then exit.
    /// Doesn't start streaming.
    #[arg(long)]
    hue_pair: bool,

    /// Hue bridge IP. Required for both pairing and streaming. Find via the
    /// Hue app (Settings → My Hue System → bridge name → i icon).
    #[arg(long)]
    hue_bridge: Option<String>,

    /// Hue "username" returned by `--hue-pair`. Identifies us to the bridge
    /// and acts as the DTLS-PSK identity.
    #[arg(long)]
    hue_app_key: Option<String>,

    /// Hue "clientkey" (32 hex chars) returned by `--hue-pair`. The DTLS PSK.
    #[arg(long)]
    hue_psk: Option<String>,

    /// Entertainment area to drive — case-insensitive name match against
    /// areas you've created in the Hue app (e.g. "Studio"). Mutually
    /// exclusive with `--hue-entertainment-id`.
    #[arg(long)]
    hue_area: Option<String>,

    /// Entertainment area UUID (alternative to `--hue-area`). Use the value
    /// printed by `--hue-pair` when name lookup is ambiguous.
    #[arg(long)]
    hue_entertainment_id: Option<String>,

    /// Master gain on the color sent to the Hue bulbs. 1.0 = unity. Lower
    /// for a softer ambient wash; higher to push the bulbs brighter than
    /// the average scene luminance suggests.
    #[arg(long, default_value_t = 1.5)]
    hue_gain: f32,

    /// Saturation boost around the dominant color's luma. 1.0 = none.
    /// Averaging across the scene desaturates colors; 1.3-1.6 punches the
    /// bulb color back toward the dominant hue.
    #[arg(long, default_value_t = 1.4)]
    hue_saturation: f32,

    /// Comma-separated channel indices that receive the SECONDARY (second-
    /// most-dominant) color from the hue histogram. Other channels get the
    /// PRIMARY color. e.g. `--hue-secondary-channels 1` for "channel 0 = TV
    /// strip (primary hue), channel 1 = ceiling bulb (secondary hue)".
    /// Empty = single-color mode (every channel gets the primary; cheaper).
    #[arg(long, default_value = "")]
    hue_secondary_channels: String,

    /// Per-channel temporal smoothing on the bulb colors (EMA at 25 Hz).
    /// 1.0 = no smoothing (every tick passes through); 0.3 ≈ 3 frames of
    /// inertia (~120 ms). Independent of the slicer's `--smoothing` —
    /// addresses fast hue flicker on the secondary bulb when two histogram
    /// bins have similar weight and frame-to-frame jitter swaps the winner.
    #[arg(long, default_value_t = 0.3)]
    hue_smoothing: f32,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    if cli.list_displays {
        list_displays();
        return;
    }

    if cli.hue_pair {
        let bridge = match cli.hue_bridge.as_deref() {
            Some(b) => b,
            None => {
                eprintln!("--hue-pair requires --hue-bridge <ip>");
                std::process::exit(1);
            }
        };
        if let Err(e) = hue::pair_and_list(bridge) {
            eprintln!("hue pairing failed: {e}");
            std::process::exit(1);
        }
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
    let dynamic = DynamicConfig::from_cli(&cli);
    // Snapshot the boot-time config so the GUI's "reset to defaults" button
    // can restore it without us having to re-parse the CLI.
    let defaults = dynamic.clone();
    let state = Arc::new(SharedState {
        device,
        controller: Mutex::new(controller),
        slicer,
        retained: Mutex::new(Vec::with_capacity(4)),
        last_source_texture: Mutex::new(None),
        config: RwLock::new(dynamic),
        hdr: cli.hdr,
        frames_seen: AtomicU64::new(0),
    });

    // SIGINT (Ctrl+C from the launching terminal) and SIGTERM (e.g.
    // `killall tv-led-mirror`) both run our atexit blackout via std::exit.
    // Cmd+Q / "Quit" from the menu also routes through exit(), so the
    // blackout fires on every shutdown path. NSApp.run() blocks the main
    // thread, so we can't unblock it from a signal handler — exit is the
    // simplest correct behavior.
    ctrlc::set_handler(|| std::process::exit(0)).expect("install signal handler");

    // ScreenCaptureKit replaces the deprecated CGDisplayStream so we can
    // request HDR + 16-bit float when --hdr is set, and so we get proper
    // wide-gamut color metadata. The capture::start blocks until the stream
    // start completion handler fires (or 5s timeout).
    if let Err(e) = capture::start(display_id, cap_w, cap_h, cli.hdr, cli.p3, state.clone()) {
        eprintln!();
        eprintln!("ScreenCaptureKit start failed: {e}");
        eprintln!();
        eprintln!("Most common cause: Screen Recording permission denied for TVLEDMirror.");
        eprintln!("  1. Launch via the .app bundle so the prompt is attributed to");
        eprintln!("     TVLEDMirror (not to your terminal):");
        eprintln!("       ./tv-led-mirror/run.sh --display <id> [other flags]");
        eprintln!("  2. Approve the Screen Recording prompt for TVLEDMirror.");
        eprintln!("  3. If no prompt appears: open System Settings → Privacy &");
        eprintln!("     Security → Screen Recording, look for TVLEDMirror, enable it.");
        eprintln!();
        std::process::exit(1);
    }
    log::info!(
        "Capture started ({}). Quit via the menu-bar item, Cmd+Q, or Ctrl+C.",
        if cli.hdr { "HDR / 16-bit float" } else { "SDR / BGRA8" }
    );

    // Polling lives on a background thread so the main thread is free for the
    // AppKit run loop (NSApplication.run requires the main thread).
    {
        let state = state.clone();
        std::thread::spawn(move || run_polling_loop(state));
    }

    // Heartbeat: re-dispatch the slicer + LED controller every ~33 ms even
    // if SCK hasn't delivered a new frame. SCK suppresses identical frames
    // when content is static (paused video, idle desktop), and without this
    // the LEDs lock to whatever the last delivered frame produced — slider
    // changes have no visible effect until content changes again.
    {
        let state = state.clone();
        std::thread::spawn(move || run_heartbeat(state));
    }

    // Watch flags.conf for edits and hot-swap the live-tunable knobs.
    reload::spawn(state.clone());

    // Always spawn the hue runtime — it idles cheaply if no bridge is
    // configured yet, and the GUI's pairing wizard will hand it a config
    // as soon as the user finishes setup.
    let initial_hue = cli
        .hue_bridge
        .clone()
        .and_then(|bridge| match build_hue_config(&cli, &bridge) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("hue: ignoring CLI config ({e}) — pair via GUI");
                None
            }
        });
    let hue_runtime = hue::spawn_runtime(state.clone(), initial_hue);

    // Block here until the user quits. The settings GUI owns the main thread;
    // closing the window, Cmd+Q, or our SIGINT handler all reach exit(), at
    // which point the atexit hook in `shutdown` runs the controller's
    // shutdown (final blackout).
    gui::run(state, defaults, hue_runtime);
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

/// Re-runs the slicer + LED controller against the cached source texture
/// at ~30 Hz. Compensates for SCK suppressing identical-content frames so
/// that GUI slider changes apply to static screens too.
///
/// Safe to run alongside `handle_capture_frame`: Metal serializes work on
/// the same command queue, and both paths take fresh encoders + a brief
/// `last_source_texture` lock.
fn run_heartbeat(state: Arc<SharedState>) -> ! {
    let interval = Duration::from_millis(33);
    loop {
        std::thread::sleep(interval);
        // Hold the lock for the whole dispatch — texture must outlive the
        // encoder, and we don't want a new captured frame to swap underneath
        // us (would also be safe via Metal's automatic retain, but cleaner
        // to serialize). Lock is held for ~1 ms.
        let guard = state.last_source_texture.lock();
        let Some(texture) = guard.as_ref() else {
            continue; // No frame captured yet.
        };
        let cfg: DynamicConfig = state.config.read().clone();
        let mut enc = state.device.create_encoder("tv-led-mirror.heartbeat");
        state.slicer.dispatch(
            &state.device,
            &mut enc,
            texture,
            cfg.blur_radius,
            cfg.luminance_floor,
            cfg.luminance_knee,
            cfg.saturation_floor,
            cfg.saturation_knee,
            cfg.crop,
            cfg.grade,
        );
        enc.commit();
        state.controller.lock().process_frame(
            &state.device,
            state.slicer.output(),
            1,
            cfg.brightness,
        );
    }
}

// ─── Shared state + frame plumbing ───────────────────────────────────────────

pub(crate) struct SharedState {
    device: GpuDevice,
    controller: Mutex<LedOutputController>,
    slicer: Slicer,
    /// IOSurfaces we've handed to the GPU but not yet released. Each entry is a
    /// retained `IOSurfaceRef` plus the wall-clock time we submitted its work,
    /// used to defer release until the GPU has plausibly finished reading it.
    retained: Mutex<Vec<RetainedSurface>>,
    /// Most-recent IOSurface-backed source texture, kept so the heartbeat
    /// thread can re-dispatch the slicer with current settings even when
    /// SCK isn't delivering new frames (e.g. content is static — a paused
    /// video, a steady desktop). Without this the LEDs lock to the last
    /// captured frame and slider drags have no visible effect.
    ///
    /// Replaced on every captured frame; the previous texture's drop
    /// releases its IOSurface back to the OS pool.
    last_source_texture: Mutex<Option<GpuTexture>>,
    /// Live-tunable knobs, hot-swapped by the flags.conf watcher thread.
    pub(crate) config: RwLock<DynamicConfig>,
    /// Capture-time format setting; switches the IOSurface→GpuTexture format
    /// each frame. Not live-tunable (would require recreating the SCK stream).
    hdr: bool,
    frames_seen: AtomicU64,
}

/// Subset of CLI knobs that the live-reload watcher can swap in on the fly.
/// Excludes capture/transport settings (display, ip, port, strips, leds,
/// hdr, p3, hue bridge/key/area) which need a recreated stream / controller
/// / socket.
#[derive(Clone, PartialEq)]
pub(crate) struct DynamicConfig {
    pub brightness: f32,
    pub blur_radius: f32,
    pub luminance_floor: f32,
    pub luminance_knee: f32,
    pub saturation_floor: f32,
    pub saturation_knee: f32,
    pub crop: Crop,
    pub grade: ColorGrade,
    pub hue: hue::HueDynamic,
}

impl DynamicConfig {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            brightness: cli.brightness.clamp(0.0, 1.0),
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
            grade: ColorGrade {
                vibrance: cli.vibrance.max(0.0),
                gamma: cli.gamma.max(0.0001),
                saturation_bias: cli.saturation_bias.max(0.0),
                peak_bias: cli.peak_bias.max(0.0),
                wb_r: cli.wb_r.max(0.0),
                wb_g: cli.wb_g.max(0.0),
                wb_b: cli.wb_b.max(0.0),
                max_luminance: cli.max_luminance.clamp(0.0, 1.0),
                black_floor: cli.black_floor.clamp(0.0, 1.0),
                hdr_peak: if cli.hdr { cli.hdr_peak.max(1.0) } else { 1.0 },
                smoothing_alpha: cli.smoothing.clamp(0.01, 1.0),
                apply_p3_to_srgb: cli.p3,
            },
            hue: hue::HueDynamic {
                gain: cli.hue_gain.clamp(0.0, 8.0),
                saturation: cli.hue_saturation.clamp(0.0, 4.0),
                secondary_channel_mask: parse_channel_mask(&cli.hue_secondary_channels),
                smoothing: cli.hue_smoothing.clamp(0.01, 1.0),
            },
        }
    }

    /// Render this config back to CLI-style flag lines suitable for inclusion
    /// in flags.conf. Used by the GUI's debounced auto-save so every slider
    /// change persists across restarts.
    ///
    /// Only includes the *live-tunable* flags (the ones in `DynamicConfig`);
    /// capture/transport flags (`--display`, `--ip`, `--port`, `--strips`,
    /// `--leds`, `--hdr`, `--p3`, `--hue-bridge`, `--hue-app-key`,
    /// `--hue-psk`, `--hue-area`) are deliberately omitted so this writer
    /// can't accidentally clobber them.
    pub(crate) fn to_flag_lines(&self) -> Vec<String> {
        let mut out = vec![
            format!("--brightness {}", self.brightness),
            format!("--blur-radius {}", self.blur_radius),
            format!("--luminance-floor {}", self.luminance_floor),
            format!("--luminance-knee {}", self.luminance_knee),
            format!("--saturation-floor {}", self.saturation_floor),
            format!("--saturation-knee {}", self.saturation_knee),
            format!("--crop-left {}", self.crop.left),
            format!("--crop-right {}", self.crop.right),
            format!("--crop-top {}", self.crop.top),
            format!("--crop-bottom {}", self.crop.bottom),
            format!("--vibrance {}", self.grade.vibrance),
            format!("--gamma {}", self.grade.gamma),
            format!("--saturation-bias {}", self.grade.saturation_bias),
            format!("--peak-bias {}", self.grade.peak_bias),
            format!("--wb-r {}", self.grade.wb_r),
            format!("--wb-g {}", self.grade.wb_g),
            format!("--wb-b {}", self.grade.wb_b),
            format!("--max-luminance {}", self.grade.max_luminance),
            format!("--black-floor {}", self.grade.black_floor),
            format!("--hdr-peak {}", self.grade.hdr_peak),
            format!("--smoothing {}", self.grade.smoothing_alpha),
            format!("--hue-gain {}", self.hue.gain),
            format!("--hue-saturation {}", self.hue.saturation),
            format!("--hue-smoothing {}", self.hue.smoothing),
        ];
        // Empty mask = single-color mode = clap's default. Omit the line
        // entirely to keep flags.conf tidy.
        if self.hue.secondary_channel_mask != 0 {
            out.push(format!(
                "--hue-secondary-channels {}",
                channel_mask_to_csv(self.hue.secondary_channel_mask)
            ));
        }
        out
    }
}

fn channel_mask_to_csv(mask: u32) -> String {
    let mut parts = Vec::new();
    for i in 0..32 {
        if mask & (1 << i) != 0 {
            parts.push(i.to_string());
        }
    }
    parts.join(",")
}

struct RetainedSurface {
    surface: *const c_void,
    submitted_at: Instant,
}

// IOSurfaceRef is a raw pointer to an opaque CF type; manual Send is fine
// because we only touch it under `retained`'s mutex.
unsafe impl Send for RetainedSurface {}

/// Called by `capture::dispatch_frame` for every CMSampleBuffer ScreenCaptureKit
/// hands us. `width`/`height` come from the CVPixelBuffer (may differ from the
/// requested cap_w/cap_h after macOS's own scaling); they're authoritative.
pub(crate) fn handle_capture_frame(
    state: &Arc<SharedState>,
    surface: *const c_void,
    width: u32,
    height: u32,
) {
    // Bump use count + CFRetain so the framework's pool can't recycle the
    // backing memory while the GPU is still reading from it.
    unsafe {
        ffi::IOSurfaceIncrementUseCount(surface);
        ffi::CFRetain(surface);
    }

    // Pick the texture format that matches the IOSurface's pixel format:
    // - HDR path: 16-bit float per channel (RGhA), already linear extended-range.
    // - SDR path: BGRA8 with sRGB hardware decode in the sampler.
    let format = if state.hdr {
        GpuTextureFormat::Rgba16Float
    } else {
        GpuTextureFormat::Bgra8UnormSrgb
    };
    let texture = unsafe {
        state.device.create_texture_from_io_surface(
            surface,
            width,
            height,
            format,
            GpuTextureUsage::SHADER_READ,
        )
    };

    // Snapshot the live-tunable config under the read lock; release the
    // lock before any GPU work so the watcher can hot-swap without blocking.
    let cfg: DynamicConfig = state.config.read().clone();

    // 1. Slice + blur + grade. The slicer auto-builds a mip pyramid of the
    //    source on first frame so each tap can sample at the correct LOD
    //    (proper area integration), then ping-pongs strip×LED outputs for
    //    temporal smoothing.
    {
        let mut enc = state.device.create_encoder("tv-led-mirror.slice");
        state.slicer.dispatch(
            &state.device,
            &mut enc,
            &texture,
            cfg.blur_radius,
            cfg.luminance_floor,
            cfg.luminance_knee,
            cfg.saturation_floor,
            cfg.saturation_knee,
            cfg.crop,
            cfg.grade,
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
        cfg.brightness,
    );
    state.frames_seen.fetch_add(1, Ordering::Relaxed);

    state.retained.lock().push(RetainedSurface {
        surface,
        submitted_at: Instant::now(),
    });
    // Cache the texture so the heartbeat thread can re-dispatch the
    // slicer when SCK isn't delivering new frames (static content). The
    // previous texture's drop releases its IOSurface back to the pool —
    // the new one inherits the "currently held" slot.
    *state.last_source_texture.lock() = Some(texture);
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

/// Parse a comma-separated channel-index list ("0,2,3") into a bitmask
/// suitable for `HueDynamic::secondary_channel_mask`. Invalid entries and
/// channel indices ≥ 32 are silently ignored — no point failing the whole
/// flag parse over a typo.
fn parse_channel_mask(s: &str) -> u32 {
    let mut mask = 0u32;
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Ok(i) = part.parse::<u32>()
            && i < 32
        {
            mask |= 1 << i;
        }
    }
    mask
}

/// Assemble the static Hue config from CLI flags. Returns `Ok(None)` if the
/// user passed `--hue-bridge` but is missing the credentials or area
/// (so streaming is silently disabled — they probably haven't paired yet),
/// `Err` if the bridge was reachable but rejected the area lookup.
fn build_hue_config(cli: &Cli, bridge: &str) -> Result<Option<hue::HueConfig>, String> {
    let (Some(app_key), Some(psk)) = (cli.hue_app_key.as_deref(), cli.hue_psk.as_deref()) else {
        log::info!("hue: --hue-bridge set but --hue-app-key/--hue-psk missing — run --hue-pair");
        return Ok(None);
    };
    let area = match (cli.hue_area.as_deref(), cli.hue_entertainment_id.as_deref()) {
        (Some(a), _) => a,
        (None, Some(id)) => id,
        (None, None) => {
            log::info!(
                "hue: --hue-area or --hue-entertainment-id required to stream — run --hue-pair to list"
            );
            return Ok(None);
        }
    };
    let (id, channel_count, area_name) = hue::resolve_area(bridge, app_key, area)?;
    Ok(Some(hue::HueConfig {
        bridge_ip: bridge.to_string(),
        app_key: app_key.to_string(),
        psk_hex: psk.to_string(),
        entertainment_id: id,
        area_name,
        channel_count,
    }))
}

