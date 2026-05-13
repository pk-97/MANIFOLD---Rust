//! Settings window — eframe/egui panel with sliders for every live-tunable
//! knob, plus a setup wizard for the Hue Entertainment connection.
//!
//! ## Slider section
//!
//! All sliders mutate the shared `RwLock<DynamicConfig>` directly, so changes
//! take effect on the next captured frame (slicer + LEDs) and the next Hue
//! tick. The flags.conf file watcher writes into the same lock, so the two
//! input paths coexist — editing flags.conf live-updates the slider
//! positions, and slider drags don't fight the file.
//!
//! "Reset to startup defaults" snapshots whatever was active when the app
//! launched (CLI flags + flags.conf at boot) and writes it back. It does
//! NOT touch flags.conf — that file remains the source of truth across
//! restarts; the reset is purely an in-memory undo.
//!
//! ## Hue setup wizard
//!
//! State machine inside the GUI tracks the user's progress: enter bridge IP
//! → press link button → wait for credentials → pick entertainment area →
//! save. Background threads (in `crate::hue`) handle the blocking HTTPS
//! polls so the UI stays responsive.
//!
//! On successful save, two things happen in parallel:
//!   1. `runtime.apply(Some(cfg))` — hue thread disconnects, reconnects with
//!      the new config immediately.
//!   2. `write_hue_creds_to_flags(...)` — flags.conf gets the four `--hue-*`
//!      lines so credentials persist across restarts.
//!
//! eframe/winit owns the main thread once `run` is called. Quit happens via
//! window close (red traffic-light) or Cmd+Q; both unwind cleanly through
//! winit and trigger our atexit hook for the final LED blackout.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::hue::{
    AreaSummary, HueConfig, HueRuntime, HueStatus, PairProgress, list_areas_background,
    pair_background, write_display_to_flags, write_hue_creds_to_flags,
    write_live_settings_to_flags,
};
use crate::{DynamicConfig, SharedState};

/// How long to wait after the last config change before persisting to
/// flags.conf. A drag of a slider produces dozens of mutations per second;
/// debouncing means we write once per gesture instead of every frame.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(750);

/// Block on the eframe event loop. Returns only on user quit; we then
/// `exit(0)` so the atexit-registered LED blackout fires.
pub fn run(state: Arc<SharedState>, defaults: DynamicConfig, hue: HueRuntime) -> ! {
    crate::shutdown::install_atexit_blackout(state.clone());

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 820.0])
            .with_min_inner_size([340.0, 480.0])
            .with_title("TVLEDMirror"),
        ..Default::default()
    };

    let last_seen = defaults.clone();
    let last_persisted = defaults.clone();
    let app = SettingsApp {
        state,
        defaults,
        hue,
        wizard: HueWizard::Idle,
        // Sticky text inputs so the bridge IP and area-name selection
        // survive UI re-renders without us threading state through every
        // single update().
        bridge_ip_input: String::new(),
        last_seen,
        last_persisted,
        dirty_since: None,
    };
    if let Err(e) = eframe::run_native(
        "TVLEDMirror",
        opts,
        Box::new(|_cc| Ok(Box::new(app))),
    ) {
        eprintln!("eframe exited with error: {e}");
    }
    std::process::exit(0);
}

struct SettingsApp {
    state: Arc<SharedState>,
    defaults: DynamicConfig,
    hue: HueRuntime,
    wizard: HueWizard,
    bridge_ip_input: String,
    /// Last `DynamicConfig` we observed — used to detect "something changed"
    /// without comparing to disk. Updated each frame.
    last_seen: DynamicConfig,
    /// Last `DynamicConfig` we wrote to flags.conf. Updated only on persist.
    /// Comparing current → last_persisted tells us if there's pending work.
    last_persisted: DynamicConfig,
    /// When the most recent change was detected, for debouncing the write.
    /// `None` means we're idle (no pending changes).
    dirty_since: Option<Instant>,
}

/// State for the Hue setup wizard, separate from the runtime's connection
/// status. The wizard drives transient UI state ("user is in the middle of
/// pairing"); the runtime drives the actual connection.
enum HueWizard {
    /// Show the runtime's status + a "Set up / change Hue" button.
    Idle,
    /// User is entering the bridge IP. We could autodiscover via mDNS but
    /// asking is simpler and avoids new deps.
    EnterBridgeIp,
    /// Background pair task is running. We poll its receiver each frame.
    Pairing {
        bridge_ip: String,
        rx: Receiver<PairProgress>,
        last_progress: PairProgress,
    },
    /// Have credentials, fetching the bridge's area list.
    LoadingAreas {
        bridge_ip: String,
        app_key: String,
        psk_hex: String,
        rx: Receiver<Result<Vec<AreaSummary>, String>>,
    },
    /// Areas loaded — user picks one and clicks Save.
    PickArea {
        bridge_ip: String,
        app_key: String,
        psk_hex: String,
        areas: Vec<AreaSummary>,
        selected: usize,
    },
    /// Wizard succeeded — show a transient confirmation (dismissible).
    SaveOk(String),
    /// Wizard failed somewhere — show error + a "Try again" button that
    /// drops back to EnterBridgeIp.
    Error(String),
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint at ~30 Hz so external state (flags.conf reload, hue
        // status, background pairing receivers) keeps the UI fresh without
        // the user having to mouse the window.
        ctx.request_repaint_after(Duration::from_millis(33));

        // Drain wizard background tasks before rendering, so the UI we
        // build this frame reflects the most recent receiver state.
        self.poll_wizard();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    self.draw_capture_error(ui);
                    self.draw_display_section(ui);
                    self.draw_hue_section(ui);
                    ui.add_space(6.0);
                    ui.separator();
                    self.draw_sliders(ui);
                });
        });

        // After rendering (so any slider mutations this frame are visible),
        // detect changes vs. last frame and either restart the debounce
        // timer or persist if the timer has elapsed. See module doc.
        self.maybe_persist_dynamic_config();
    }
}

// ─── Display picker ──────────────────────────────────────────────────────────

impl SettingsApp {
    /// Dropdown of all displays SCK can capture. Picking a different one
    /// writes `--display <id>` to flags.conf and relaunches the .app so
    /// the new ID takes effect — SCK doesn't expose a hot-swap, and the
    /// stream's display is fixed at SCContentFilter construction time.
    ///
    /// Display IDs change when the TV gets reconnected (HDMI source switch,
    /// sleep/wake, macOS update); naming via NSScreen.localizedName lets
    /// the user pick the right one without memorising the integer.
    fn draw_display_section(&mut self, ui: &mut egui::Ui) {
        if self.state.available_displays.is_empty() {
            // Empty when SCK couldn't enumerate — usually because Screen
            // Recording is denied. The capture-error banner already
            // surfaces that, no point repeating it here.
            return;
        }
        ui.add_space(4.0);
        ui.heading("Display");
        ui.add_space(2.0);

        let active_id = self.state.active_display_id;
        let active_label = self
            .state
            .available_displays
            .iter()
            .find(|d| d.id == active_id)
            .map(label_for_display)
            .unwrap_or_else(|| format!("Display {active_id} (not found)"));

        let mut clicked_id: Option<u32> = None;
        egui::ComboBox::from_id_salt("tvled-display-picker")
            .selected_text(active_label)
            .width(ui.available_width().min(360.0))
            .show_ui(ui, |ui| {
                for d in &self.state.available_displays {
                    let label = label_for_display(d);
                    if ui
                        .selectable_label(d.id == active_id, label)
                        .clicked()
                    {
                        clicked_id = Some(d.id);
                    }
                }
            });

        if let Some(new_id) = clicked_id
            && new_id != active_id
        {
            match write_display_to_flags(new_id) {
                Ok(()) => relaunch_self(),
                Err(e) => log::warn!("persist display: {e}"),
            }
        }

        ui.add_space(4.0);
        ui.separator();
    }
}

/// Format one display as a single-line user-facing label. Prefers the
/// NSScreen name; falls back to the integer ID.
fn label_for_display(d: &crate::capture::DisplayInfo) -> String {
    let name = d.name.as_deref().unwrap_or("Display");
    format!("{name} — {}×{} (id {})", d.width, d.height, d.id)
}

/// Spawn a fresh instance of TVLEDMirror.app via LaunchServices, then exit.
/// `-n` forces a new instance even if one is already running (it's us, but
/// our `exit(0)` is racing with the spawn). LaunchServices serialises so the
/// new instance ends up holding the screen-capture grant cleanly.
fn relaunch_self() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        // exe = .../TVLEDMirror.app/Contents/MacOS/tv-led-mirror
        // Walk up to the .app: parents are MacOS / Contents / TVLEDMirror.app.
        if let Some(app_path) = exe.ancestors().nth(3) {
            let _ = std::process::Command::new("open")
                .arg("-n")
                .arg(app_path)
                .spawn();
        }
    }
    std::process::exit(0);
}

// ─── Capture-error banner ────────────────────────────────────────────────────

impl SettingsApp {
    /// Show a red banner + actionable button when ScreenCaptureKit failed to
    /// start. The most common cause is Screen Recording TCC denied for this
    /// build — recoverable from System Settings without rebuilding. The
    /// "Open Privacy Settings" button jumps the user straight to the right
    /// pane via the macOS x-apple URL scheme.
    fn draw_capture_error(&mut self, ui: &mut egui::Ui) {
        let err = self.state.capture_error.read().clone();
        let Some(msg) = err else { return };
        ui.add_space(4.0);
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(56, 16, 16))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(180, 60, 60)))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Screen capture is not running")
                        .color(egui::Color32::from_rgb(255, 180, 180))
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(format!("Cause: {msg}"))
                        .color(egui::Color32::LIGHT_GRAY)
                        .small(),
                );
                ui.label(
                    egui::RichText::new(
                        "Almost always Screen Recording permission. Enable TVLEDMirror in \
                         Privacy & Security, then quit and relaunch.",
                    )
                    .color(egui::Color32::LIGHT_GRAY)
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Open Privacy Settings").clicked() {
                        // Deep-links directly to the Screen Recording pane.
                        // `open` is on every Mac; no extra deps.
                        let _ = std::process::Command::new("open")
                            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                            .spawn();
                    }
                    if ui.button("Quit").clicked() {
                        std::process::exit(0);
                    }
                });
            });
        ui.add_space(6.0);
        ui.separator();
    }
}

// ─── Persistence ────────────────────────────────────────────────────────────

impl SettingsApp {
    /// Debounced auto-save of the live-tunable `DynamicConfig` to flags.conf.
    ///
    /// Algorithm:
    ///   - Each frame, snapshot the current config.
    ///   - If it differs from `last_seen` (i.e. something just changed —
    ///     could be a slider drag, the reload thread picking up an external
    ///     edit, or our own previous write), restart the debounce timer.
    ///   - If the timer has elapsed AND the current value still differs from
    ///     `last_persisted` (i.e. we have something genuine to write),
    ///     write the file and update `last_persisted`.
    ///
    /// The reload thread will see our write on its next tick and re-parse,
    /// but the parsed values match what's already in `state.config` so the
    /// no-op assignment is harmless. Importantly, after our write
    /// `last_persisted == current`, so we don't loop into a re-write.
    fn maybe_persist_dynamic_config(&mut self) {
        let current = self.state.config.read().clone();
        if current != self.last_seen {
            self.last_seen = current.clone();
            self.dirty_since = Some(Instant::now());
        }
        if let Some(since) = self.dirty_since
            && since.elapsed() >= PERSIST_DEBOUNCE
            && current != self.last_persisted
        {
            match write_live_settings_to_flags(&current.to_flag_lines()) {
                Ok(()) => {
                    self.last_persisted = current;
                    self.dirty_since = None;
                }
                Err(e) => {
                    log::warn!("persist: {e}");
                    // Back off so we don't spam logs on a persistent failure
                    // (e.g. permissions issue). Try again in a few seconds.
                    self.dirty_since = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }
    }
}

// ─── Sliders ────────────────────────────────────────────────────────────────

impl SettingsApp {
    fn draw_sliders(&mut self, ui: &mut egui::Ui) {
        // One write lock for the whole frame. Held briefly — egui paints in
        // tens of microseconds — and the slicer / hue threads tolerate a
        // momentary read-block.
        let mut cfg = self.state.config.write();

        section(ui, "Brightness & Smoothing", |ui| {
            slider(ui, &mut cfg.brightness, 0.0..=1.0, "brightness");
            slider(
                ui,
                &mut cfg.grade.smoothing_alpha,
                0.01..=1.0,
                "smoothing α (1 = no smoothing)",
            );
        });

        section(ui, "Sampling", |ui| {
            slider(ui, &mut cfg.blur_radius, 0.0..=4.0, "blur radius");
            slider(
                ui,
                &mut cfg.grade.saturation_bias,
                0.0..=10.0,
                "saturation bias (blur taps)",
            );
            slider(
                ui,
                &mut cfg.grade.peak_bias,
                0.0..=20.0,
                "peak bias (preserves brightness under wide blur)",
            );
        });

        section(ui, "Crop (fraction of source skipped)", |ui| {
            slider(ui, &mut cfg.crop.left, 0.0..=0.49, "left");
            slider(ui, &mut cfg.crop.right, 0.0..=0.49, "right");
            slider(ui, &mut cfg.crop.top, 0.0..=0.49, "top");
            slider(ui, &mut cfg.crop.bottom, 0.0..=0.49, "bottom");
        });

        section(ui, "Gates (soft-fade dim / desaturated content)", |ui| {
            slider(ui, &mut cfg.luminance_floor, 0.0..=1.0, "luminance floor");
            slider(ui, &mut cfg.luminance_knee, 0.001..=0.5, "luminance knee");
            slider(ui, &mut cfg.saturation_floor, 0.0..=1.0, "saturation floor");
            slider(ui, &mut cfg.saturation_knee, 0.001..=0.5, "saturation knee");
        });

        section(ui, "Color Grade", |ui| {
            slider(ui, &mut cfg.grade.vibrance, 0.0..=2.0, "vibrance");
            slider(ui, &mut cfg.grade.gamma, 0.1..=4.0, "gamma");
            slider(ui, &mut cfg.grade.max_luminance, 0.0..=1.0, "max luminance");
            slider(ui, &mut cfg.grade.black_floor, 0.0..=0.2, "black floor");
            slider(ui, &mut cfg.grade.hdr_peak, 1.0..=10.0, "HDR peak");
        });

        section(ui, "White Balance", |ui| {
            slider(ui, &mut cfg.grade.wb_r, 0.0..=2.0, "R");
            slider(ui, &mut cfg.grade.wb_g, 0.0..=2.0, "G");
            slider(ui, &mut cfg.grade.wb_b, 0.0..=2.0, "B");
        });

        section(ui, "Hue Lights (live)", |ui| {
            slider(ui, &mut cfg.hue.gain, 0.0..=8.0, "gain");
            slider(ui, &mut cfg.hue.saturation, 0.0..=4.0, "saturation");
            slider(
                ui,
                &mut cfg.hue.smoothing,
                0.01..=1.0,
                "smoothing α (1 = none, lower = anti-flicker)",
            );

            // Per-channel role assignment: PRIMARY (most-dominant hue) /
            // SECONDARY (second-most-dominant hue from the histogram).
            // Only meaningful while connected — otherwise we don't know the
            // channel count, so the chips would be guesses.
            if let HueStatus::Streaming { channel_count, .. } = self.hue.status()
                && channel_count > 0
            {
                ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Per-channel role (toggle = secondary hue, else primary):",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for i in 0..channel_count {
                            let bit = 1u32 << i;
                            let mut is_secondary = (cfg.hue.secondary_channel_mask & bit) != 0;
                            let label = format!("Ch {i}");
                            if ui.toggle_value(&mut is_secondary, label).changed() {
                                if is_secondary {
                                    cfg.hue.secondary_channel_mask |= bit;
                                } else {
                                    cfg.hue.secondary_channel_mask &= !bit;
                                }
                            }
                        }
                    });
                if cfg.hue.secondary_channel_mask == 0 {
                    ui.label(
                        egui::RichText::new(
                            "Single-color mode — all channels get the dominant hue.",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                }
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        if ui
            .add_sized(
                [ui.available_width(), 32.0],
                egui::Button::new("Reset to startup defaults"),
            )
            .on_hover_text(
                "Restores values that were active at app launch \
                 (CLI flags + flags.conf as of boot). Doesn't \
                 modify flags.conf on disk.",
            )
            .clicked()
        {
            *cfg = self.defaults.clone();
        }
    }
}

// ─── Hue setup section ───────────────────────────────────────────────────────

impl SettingsApp {
    /// Pull pending messages from any active wizard receiver. Done once per
    /// frame so the UI we render reflects the freshest state.
    fn poll_wizard(&mut self) {
        // Rebind self.wizard via mem::replace so we can move out of the
        // current variant when transitioning.
        let current = std::mem::replace(&mut self.wizard, HueWizard::Idle);
        self.wizard = match current {
            HueWizard::Pairing {
                bridge_ip,
                rx,
                mut last_progress,
            } => {
                // Drain all pending messages — keep the latest.
                let mut latest = last_progress.clone();
                while let Ok(p) = rx.try_recv() {
                    latest = p;
                }
                last_progress = latest;
                match &last_progress {
                    PairProgress::Done { app_key, psk_hex } => {
                        let app_key = app_key.clone();
                        let psk_hex = psk_hex.clone();
                        // Move on to area listing.
                        let list_rx =
                            list_areas_background(bridge_ip.clone(), app_key.clone());
                        HueWizard::LoadingAreas {
                            bridge_ip,
                            app_key,
                            psk_hex,
                            rx: list_rx,
                        }
                    }
                    PairProgress::Failed(e) => HueWizard::Error(e.clone()),
                    PairProgress::Waiting { .. } => HueWizard::Pairing {
                        bridge_ip,
                        rx,
                        last_progress,
                    },
                }
            }
            HueWizard::LoadingAreas {
                bridge_ip,
                app_key,
                psk_hex,
                rx,
            } => match rx.try_recv() {
                Ok(Ok(areas)) if areas.is_empty() => HueWizard::Error(
                    "Bridge has no entertainment areas configured. Open the \
                     Hue app → Settings → Entertainment areas, create one, \
                     then try again."
                        .to_string(),
                ),
                Ok(Ok(areas)) => HueWizard::PickArea {
                    bridge_ip,
                    app_key,
                    psk_hex,
                    areas,
                    selected: 0,
                },
                Ok(Err(e)) => HueWizard::Error(format!("listing areas: {e}")),
                Err(std::sync::mpsc::TryRecvError::Empty) => HueWizard::LoadingAreas {
                    bridge_ip,
                    app_key,
                    psk_hex,
                    rx,
                },
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    HueWizard::Error("area list task disconnected".into())
                }
            },
            other => other,
        };
    }

    fn draw_hue_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Hue Setup");
        ui.add_space(2.0);

        // Always show the runtime's live status at the top.
        let status = self.hue.status();
        match &status {
            HueStatus::Disabled => {
                ui.colored_label(egui::Color32::GRAY, "● not connected");
            }
            HueStatus::Connecting { area_name } => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("● connecting to \"{area_name}\"…"),
                );
            }
            HueStatus::Streaming {
                area_name,
                bridge_ip,
                channel_count,
            } => {
                ui.colored_label(
                    egui::Color32::LIGHT_GREEN,
                    format!(
                        "● streaming → \"{area_name}\" via {bridge_ip} ({channel_count} ch)"
                    ),
                );
            }
            HueStatus::Error { message } => {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("● error: {message}"));
            }
        }

        ui.add_space(4.0);

        // Wizard / setup controls below.
        let wizard = std::mem::replace(&mut self.wizard, HueWizard::Idle);
        self.wizard = match wizard {
            HueWizard::Idle => self.draw_idle(ui, &status),
            HueWizard::EnterBridgeIp => self.draw_enter_bridge(ui),
            HueWizard::Pairing {
                bridge_ip,
                rx,
                last_progress,
            } => self.draw_pairing(ui, bridge_ip, rx, last_progress),
            HueWizard::LoadingAreas {
                bridge_ip,
                app_key,
                psk_hex,
                rx,
            } => self.draw_loading_areas(ui, bridge_ip, app_key, psk_hex, rx),
            HueWizard::PickArea {
                bridge_ip,
                app_key,
                psk_hex,
                areas,
                mut selected,
            } => {
                let next = self.draw_pick_area(
                    ui,
                    &bridge_ip,
                    &app_key,
                    &psk_hex,
                    &areas,
                    &mut selected,
                );
                next.unwrap_or(HueWizard::PickArea {
                    bridge_ip,
                    app_key,
                    psk_hex,
                    areas,
                    selected,
                })
            }
            HueWizard::SaveOk(msg) => self.draw_save_ok(ui, msg),
            HueWizard::Error(e) => self.draw_error(ui, e),
        };
    }

    fn draw_idle(&mut self, ui: &mut egui::Ui, status: &HueStatus) -> HueWizard {
        ui.horizontal(|ui| {
            let label = match status {
                HueStatus::Streaming { .. } | HueStatus::Connecting { .. } => "Change Hue setup",
                _ => "Set up Hue",
            };
            if ui.button(label).clicked() {
                self.bridge_ip_input.clear();
                return HueWizard::EnterBridgeIp;
            }
            if matches!(
                status,
                HueStatus::Streaming { .. }
                    | HueStatus::Connecting { .. }
                    | HueStatus::Error { .. }
            ) && ui
                .button("Disconnect")
                .on_hover_text(
                    "Stops the hue stream. Credentials in flags.conf are kept.",
                )
                .clicked()
            {
                self.hue.apply(None);
            }
            HueWizard::Idle
        })
        .inner
    }

    fn draw_enter_bridge(&mut self, ui: &mut egui::Ui) -> HueWizard {
        ui.label("Bridge IP address:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.bridge_ip_input)
                .hint_text("e.g. 192.168.1.50"),
        );
        ui.label(
            egui::RichText::new(
                "Find via the Hue app → Settings → My Hue System → bridge → ⓘ",
            )
            .small()
            .color(egui::Color32::GRAY),
        );
        ui.add_space(4.0);

        let submit = (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            || ui.button("Pair").clicked();
        let cancelled = ui.button("Cancel").clicked();

        if cancelled {
            return HueWizard::Idle;
        }
        if submit {
            let ip = self.bridge_ip_input.trim().to_string();
            if ip.is_empty() {
                return HueWizard::EnterBridgeIp;
            }
            let rx = pair_background(ip.clone());
            return HueWizard::Pairing {
                bridge_ip: ip,
                rx,
                last_progress: PairProgress::Waiting { seconds_elapsed: 0 },
            };
        }
        HueWizard::EnterBridgeIp
    }

    fn draw_pairing(
        &mut self,
        ui: &mut egui::Ui,
        bridge_ip: String,
        rx: Receiver<PairProgress>,
        last_progress: PairProgress,
    ) -> HueWizard {
        ui.label(
            egui::RichText::new(format!("Press the LINK button on the Hue bridge ({bridge_ip})"))
                .strong(),
        );
        if let PairProgress::Waiting { seconds_elapsed } = &last_progress {
            ui.label(format!(
                "waiting… ({seconds_elapsed}s elapsed, gives up after 60s)"
            ));
            ui.spinner();
        }
        if ui.button("Cancel").clicked() {
            // Just drop the receiver; the background thread will finish on
            // its own and its message goes nowhere.
            return HueWizard::Idle;
        }
        HueWizard::Pairing {
            bridge_ip,
            rx,
            last_progress,
        }
    }

    fn draw_loading_areas(
        &mut self,
        ui: &mut egui::Ui,
        bridge_ip: String,
        app_key: String,
        psk_hex: String,
        rx: Receiver<Result<Vec<AreaSummary>, String>>,
    ) -> HueWizard {
        ui.label("Paired! Loading entertainment areas…");
        ui.spinner();
        HueWizard::LoadingAreas {
            bridge_ip,
            app_key,
            psk_hex,
            rx,
        }
    }

    fn draw_pick_area(
        &mut self,
        ui: &mut egui::Ui,
        bridge_ip: &str,
        app_key: &str,
        psk_hex: &str,
        areas: &[AreaSummary],
        selected: &mut usize,
    ) -> Option<HueWizard> {
        ui.label("Select an entertainment area:");
        // Clamp selection in case the area list ever changes underneath us.
        if *selected >= areas.len() {
            *selected = 0;
        }
        let label_for = |i: usize| -> String {
            let a = &areas[i];
            format!("{} ({} channels)", a.name, a.channel_count)
        };
        egui::ComboBox::from_id_salt("hue-area-combo")
            .selected_text(label_for(*selected))
            .show_ui(ui, |ui| {
                for (i, _a) in areas.iter().enumerate() {
                    ui.selectable_value(selected, i, label_for(i));
                }
            });

        ui.add_space(6.0);
        let mut next: Option<HueWizard> = None;
        ui.horizontal(|ui| {
            if ui.button("Save & Connect").clicked() {
                let area = &areas[*selected];
                // Persist to flags.conf (so credentials survive restart).
                if let Err(e) =
                    write_hue_creds_to_flags(bridge_ip, app_key, psk_hex, &area.name)
                {
                    next = Some(HueWizard::Error(format!("saving flags.conf: {e}")));
                    return;
                }
                // Hand the runtime the new config — it'll disconnect the
                // current stream (if any) and reconnect within ~1 s.
                self.hue.apply(Some(HueConfig {
                    bridge_ip: bridge_ip.to_string(),
                    app_key: app_key.to_string(),
                    psk_hex: psk_hex.to_string(),
                    entertainment_id: area.id.clone(),
                    area_name: area.name.clone(),
                    channel_count: area.channel_count,
                }));
                next = Some(HueWizard::SaveOk(format!(
                    "Connected to \"{}\". Credentials saved to flags.conf.",
                    area.name
                )));
            }
            if ui.button("Cancel").clicked() {
                next = Some(HueWizard::Idle);
            }
        });
        next
    }

    fn draw_save_ok(&mut self, ui: &mut egui::Ui, msg: String) -> HueWizard {
        ui.colored_label(egui::Color32::LIGHT_GREEN, &msg);
        if ui.button("OK").clicked() {
            return HueWizard::Idle;
        }
        HueWizard::SaveOk(msg)
    }

    fn draw_error(&mut self, ui: &mut egui::Ui, e: String) -> HueWizard {
        ui.colored_label(egui::Color32::LIGHT_RED, &e);
        if ui.button("Try again").clicked() {
            self.bridge_ip_input.clear();
            return HueWizard::EnterBridgeIp;
        }
        if ui.button("Dismiss").clicked() {
            return HueWizard::Idle;
        }
        HueWizard::Error(e)
    }
}

// ─── small helpers to keep the slider list readable ─────────────────────────

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.heading(title);
    body(ui);
    ui.add_space(4.0);
    ui.separator();
}

fn slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    label: &str,
) {
    ui.add(
        egui::Slider::new(value, range)
            .text(label)
            .clamping(egui::SliderClamping::Always),
    );
}
