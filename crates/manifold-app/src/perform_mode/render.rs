//! Perform-mode rendering — minimal main-window draw path.

use manifold_core::types::ClockAuthority;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::FontWeight;

use crate::app::Application;
use crate::perform_mode::{cue, macros as perform_macros, tracks};

/// Read-only sync status snapshot for the perform HUD.
struct SyncStatus {
    authority: ClockAuthority,
    link_enabled: bool,
    link_peers: i32,
    midi_clock_enabled: bool,
    midi_clock_receiving: bool,
    midi_clock_position_display: String,
    midi_clock_device_name: String,
}

impl Application {
    /// Performance mode tick: drains content state, polls output window
    /// liveness, processes only the exit button, and renders the perform
    /// HUD on the main window. The content thread and output window are
    /// completely untouched.
    pub(crate) fn tick_perform_mode(&mut self) {
        // 1. Drain content state channel so the content thread isn't blocked
        //    on backpressure. We keep the latest state for the HUD readout
        //    (BPM, current beat, Ableton session) but DO NOT push anything
        //    to panels — the normal UI is frozen.
        //
        // The Ableton session is published edge-triggered (Some only when it
        // changes, None otherwise). The persistent cache lives at
        // `ui_root.ableton_session` and is normally maintained by state_sync,
        // which doesn't run in perform mode — so we update it ourselves here
        // whenever a non-None session arrives.
        if let Some(ref rx) = self.state_rx {
            while let Ok(state) = rx.try_recv() {
                if let Some(session) = &state.ableton_session {
                    self.ui_root.ableton_session = Some(std::sync::Arc::clone(session));
                }
                self.content_state = state;
            }
        }

        // 2. Backup auto-exit: if the output window vanished (display unplug,
        //    crash, etc.) and the explicit close hooks didn't catch it, exit.
        if !self.window_registry.has_output_window() {
            self.perform.pending_exit = true;
            return;
        }

        // 3. Render the perform-mode screen on the main window.
        let Some(gpu) = &self.gpu else { return };
        let scale = self.scale_factor;
        let Some(window_id) = self.primary_window_id else {
            return;
        };
        let surface_dims = self
            .window_registry
            .get(&window_id)
            .and_then(|ws| ws.surface.as_ref())
            .map(|s| (s.width, s.height))
            .unwrap_or((1, 1));
        let (surface_w, surface_h) = surface_dims;
        let Some(offscreen) = &self.ui_offscreen else {
            return;
        };
        if offscreen.width != surface_w || offscreen.height != surface_h {
            return;
        }

        let logical_w = (surface_w as f64 / scale) as u32;
        let logical_h = (surface_h as f64 / scale) as u32;
        let lw = logical_w as f32;
        let lh = logical_h as f32;

        // Exit button — small and intentionally hard to hit accidentally.
        // Bottom-center with generous padding above. Tighter dimensions
        // and smaller font; the click target is still finger-sized but
        // it does NOT dominate the screen the way an emergency button
        // might (which would invite accidental presses during a show).
        let btn_w = 180.0_f32;
        let btn_h = 36.0_f32;
        let btn_x = (lw - btn_w) * 0.5;
        let btn_y = lh - btn_h - 24.0;
        self.perform.exit_button_rect =
            manifold_ui::node::Rect::new(btn_x, btn_y, btn_w, btn_h);

        // Snapshot HUD inputs from content_state + persistent ui_root cache.
        let current_beat = self.content_state.current_beat.0;
        let bpm = self.content_state.bpm;
        let beats_per_bar = self.content_state.time_signature_numerator.max(1) as u32;
        let is_playing = self.content_state.is_playing;
        let ableton_connected = self.content_state.ableton_connected;
        let sync_status = SyncStatus {
            authority: self.content_state.clock_authority,
            link_enabled: self.content_state.link_enabled,
            link_peers: self.content_state.link_peers,
            midi_clock_enabled: self.content_state.midi_clock_enabled,
            midi_clock_receiving: self.content_state.midi_clock_receiving,
            midi_clock_position_display: self
                .content_state
                .midi_clock_position_display
                .clone(),
            midi_clock_device_name: self
                .content_state
                .midi_clock_device_name
                .clone(),
        };
        // Snapshot the session Arc (atomic refcount bump, zero data copy).
        // All cue/track/macro reads below borrow from this snapshot, avoiding
        // per-frame clones of cue_points, track names, etc.
        let session_snapshot = self.ui_root.ableton_session.clone();
        let cue_points: &[manifold_playback::ableton_bridge::CuePoint] = session_snapshot
            .as_ref()
            .map(|s| s.cue_points.as_slice())
            .unwrap_or(&[]);
        let analysis = cue::analyze(cue_points, current_beat);
        let current_name = analysis
            .current
            .map(|c| c.name.as_str())
            .unwrap_or("—");
        let next_name = analysis
            .next
            .map(|c| c.name.as_str())
            .unwrap_or("—");
        let countdown = analysis
            .beats_to_next
            .map(|b| cue::format_countdown(b, beats_per_bar));
        let section_progress = cue::section_progress(
            analysis.current,
            analysis.next,
            current_beat,
        );
        let bar_beat = cue::format_bar_beat(current_beat, beats_per_bar);

        // Tracks playing in the *current* section: any non-muted clip
        // overlapping `[current.time, next.time)` (variant (a)).
        let now_section_tracks: Vec<&str> = if let Some(cur_cue) = analysis.current {
            let section_end = analysis
                .next
                .map(|c| c.time)
                .unwrap_or(f64::INFINITY);
            session_snapshot
                .as_ref()
                .and_then(|s| s.play_group.as_ref())
                .map(|g| {
                    g.tracks
                        .iter()
                        .filter(|t| tracks::plays_in_range(t, cur_cue.time, section_end))
                        .map(|t| t.name.as_str())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Tracks playing in the *next* section (variant (a): any non-muted
        // clip overlapping `[next.time, section_end)`, including straddlers).
        // Section end = the cue after `next`, or +∞ if `next` is the last.
        let next_section_tracks: Vec<&str> = if let Some(next_cue) = analysis.next {
            let section_end = cue_points
                .iter()
                .find(|c| c.time > next_cue.time)
                .map(|c| c.time)
                .unwrap_or(f64::INFINITY);
            session_snapshot
                .as_ref()
                .and_then(|s| s.play_group.as_ref())
                .map(|g| {
                    g.tracks
                        .iter()
                        .filter(|t| tracks::plays_in_range(t, next_cue.time, section_end))
                        .map(|t| t.name.as_str())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

// Macros: snapshot mapped Ableton macros for the left column.
        // Top-N for now to avoid overflowing tall projects with many macros.
        const MACRO_DISPLAY_LIMIT: usize = 12;
        let macros_snapshot: Vec<perform_macros::MacroDisplay> = session_snapshot
            .as_ref()
            .map(|s| {
                perform_macros::snapshot(&self.local_project, s, MACRO_DISPLAY_LIMIT)
            })
            .unwrap_or_default();

        // Skip drawable acquisition on resize frames (same rule as the
        // normal present path).
        if self.surface_resized_this_frame {
            self.surface_resized_this_frame = false;
            return;
        }

        let mut encoder = gpu.device.create_encoder("Perform Frame");
        // Clear the offscreen to pure black.
        encoder.clear_texture(offscreen, 0.0, 0.0, 0.0, 1.0);

        // Build immediate-mode draws into the UIRenderer.
        if let Some(ui) = &mut self.ui_renderer {
            ui.begin_frame();

            draw_cue_hud(
                ui,
                lw,
                lh,
                current_name,
                next_name,
                countdown.as_ref(),
                section_progress,
                &bar_beat,
                &now_section_tracks,
                &next_section_tracks,
                bpm,
                is_playing,
                ableton_connected,
                cue_points.is_empty(),
            );

            draw_sync_indicators(ui, &sync_status);

            if !macros_snapshot.is_empty() {
                draw_macros_column(ui, lh, &macros_snapshot);
            }

            draw_exit_button(
                ui,
                btn_x,
                btn_y,
                btn_w,
                btn_h,
                self.perform.exit_button_hover,
            );

            // Flush.
            if ui.prepare(&gpu.device, logical_w, logical_h, scale) {
                ui.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
            }
        }

        // Blit compositor output preview into the right column, vertically
        // centered. Reads the latest frame from the preview IOSurface bridge
        // (same source the workspace viewport uses). Zero extra GPU work —
        // the frame is already rendered by the content thread.
        #[cfg(target_os = "macos")]
        if let (Some(bridge), Some(blit_pipeline), Some(blit_sampler)) = (
            &self.preview_texture_bridge,
            &self.blit_pipeline,
            &self.blit_sampler,
        ) {
            // Re-import textures on bridge resize (rare).
            let bridge_gen = bridge.generation();
            if bridge_gen != self.last_preview_bridge_generation {
                self.last_preview_bridge_generation = bridge_gen;
                let textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] =
                    std::array::from_fn(|i| unsafe {
                        bridge.import_texture_native(&gpu.device, i)
                    });
                self.ui_preview_textures = textures.map(Some);
            }
            let front = bridge.front_index() as usize;
            if let Some(source) = self.ui_preview_textures[front].as_ref() {
                let (comp_w, comp_h) = self
                    .content_pipeline_output
                    .as_ref()
                    .map(|p| p.get_dimensions())
                    .unwrap_or((1920, 1080));
                let source_aspect = comp_w as f32 / comp_h as f32;

                // Right column: mirror the macros column width on the left.
                let preview_max_w = 320.0_f32;
                let right_pad = 48.0_f32;
                let preview_x = lw - right_pad - preview_max_w;
                let preview_max_h = preview_max_w / source_aspect;
                // Vertically center in the window.
                let preview_y = (lh - preview_max_h) * 0.5;

                // Aspect-fit (source is wider or taller than the box).
                let box_aspect = preview_max_w / preview_max_h;
                let (fit_w, fit_h) = if source_aspect > box_aspect {
                    (preview_max_w, preview_max_w / source_aspect)
                } else {
                    (preview_max_h * source_aspect, preview_max_h)
                };
                let fit_x = preview_x + (preview_max_w - fit_w) * 0.5;
                let fit_y = preview_y + (preview_max_h - fit_h) * 0.5;

                // Convert logical → physical pixels for the viewport.
                let sf = scale as f32;
                encoder.draw_fullscreen_viewport(
                    blit_pipeline,
                    offscreen,
                    &[
                        manifold_gpu::GpuBinding::Texture {
                            binding: 0,
                            texture: source,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 1,
                            sampler: blit_sampler,
                        },
                    ],
                    (fit_x * sf, fit_y * sf, fit_w * sf, fit_h * sf),
                    manifold_gpu::GpuLoadAction::Load,
                    "Perform Preview Blit",
                );
            }
        }

        encoder.commit();

        // Acquire drawable and present.
        let drawable = {
            let ws = match self.window_registry.get_mut(&window_id) {
                Some(ws) => ws,
                None => return,
            };
            let surface = match ws.surface.as_ref() {
                Some(s) => s,
                None => return,
            };
            match surface.next_drawable() {
                Some(d) => d,
                None => return,
            }
        };
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        let blit_pipeline = match &self.blit_pipeline {
            Some(p) => p,
            None => return,
        };
        let blit_sampler = match &self.blit_sampler {
            Some(s) => s,
            None => return,
        };
        let mut present_enc = gpu.device.create_encoder("Perform Present");
        present_enc.draw_fullscreen(
            blit_pipeline,
            &drawable_tex,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: offscreen,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler: blit_sampler,
                },
            ],
            false,
            true,
            "Offscreen → Drawable (Perform)",
        );
        present_enc.present_drawable(&drawable);
        present_enc.commit();

        self.frame_count += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────
// Fixed-column numeric text helpers
//
// Inter (the UI font) is proportional, so "1" is narrower than "8". When
// you re-render a number every frame the rendered width changes from
// frame to frame, which creates position jitter no matter how you anchor
// the result. The fix: render each digit at a fixed advance equal to the
// width of the widest digit ("0" works), so a numeric string has constant
// pixel width regardless of which digits it contains. The price is a tiny
// optical asymmetry on narrow digits like "1" — invisible at performance
// distance, dwarfed by the stability win.
// ─────────────────────────────────────────────────────────────────────

/// Compute the rendered width of a numeric string when drawn with
/// fixed-column digits. Result is constant for any input of the same
/// (digit_count, non_digit_chars).
fn numeric_text_width(ui: &mut UIRenderer, text: &str, font_size: u16) -> f32 {
    let digit_w = ui.measure_text_cached("0", font_size, FontWeight::Medium).x;
    let mut w = 0.0;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            w += digit_w;
        } else {
            let s = ch.encode_utf8(&mut buf);
            w += ui.measure_text_cached(s, font_size, FontWeight::Medium).x;
        }
    }
    w
}

/// Draw sync source indicators in the top-left, matching the standard
/// transport bar style: colored badge + green dot + status text.
///
/// Layout (left-to-right):
///   SRC:CLK   LINK  ● 1 peer   CLK  IAC Driver Bus 1  ● 121.2.2
///
/// Read-only — no click handling in perform mode.
fn draw_sync_indicators(ui: &mut UIRenderer, sync: &SyncStatus) {
    // Color palette — matches transport bar exactly.
    let link_orange_bg = [0.75, 0.48, 0.08, 1.0];
    let midi_purple_bg = [0.58, 0.30, 0.58, 1.0];
    let inactive_bg = [0.23, 0.23, 0.24, 1.0];
    let osc_blue_bg = [0.22, 0.52, 0.70, 1.0];
    let white = [240u8, 240u8, 240u8, 255u8];
    let dimmed = [140u8, 140u8, 145u8, 255u8];
    let dot_green = [64u8, 179u8, 77u8, 255u8];
    let dot_yellow = [204u8, 166u8, 38u8, 255u8];
    let dot_inactive = [64u8, 64u8, 69u8, 255u8];

    let badge_font: u16 = 11;
    let status_font: u16 = 11;
    let badge_h = 22.0_f32;
    let badge_pad_h = 10.0_f32; // horizontal padding inside badge
    let badge_radius = 4.0_f32;
    let dot_size = 8.0_f32;
    let item_gap = 6.0_f32; // gap between badge and dot
    let text_gap = 5.0_f32; // gap between dot and status text
    let group_gap = 14.0_f32; // gap between groups (LINK group, CLK group)

    let left_pad = 48.0_f32;
    let top_pad = 16.0_f32;

    let mut x = left_pad;
    let y = top_pad;
    let text_y = y + (badge_h - badge_font as f32) * 0.5;
    let dot_y = y + (badge_h - dot_size) * 0.5;

    // Helper: draw a badge (rounded rect with centered text).
    let draw_badge = |ui: &mut UIRenderer, x: f32, label: &str, bg: [f32; 4]| -> f32 {
        let text_w = ui.measure_text_cached(label, badge_font, FontWeight::Medium).x;
        let w = text_w + badge_pad_h * 2.0;
        ui.draw_rounded_rect(x, y, w, badge_h, bg, badge_radius);
        ui.draw_text(x + badge_pad_h, text_y, label, badge_font as f32, white);
        w
    };

    // Helper: draw a status dot.
    let draw_dot = |ui: &mut UIRenderer, x: f32, color: [u8; 4]| {
        let c = [
            color[0] as f32 / 255.0,
            color[1] as f32 / 255.0,
            color[2] as f32 / 255.0,
            1.0,
        ];
        ui.draw_rounded_rect(x, dot_y, dot_size, dot_size, c, dot_size * 0.5);
    };

    // ── SRC badge (clock authority) ─────────────────────────────────
    let auth_label = sync.authority.transport_label();
    let auth_bg = match sync.authority {
        ClockAuthority::Internal => inactive_bg,
        ClockAuthority::Link => link_orange_bg,
        ClockAuthority::MidiClock => midi_purple_bg,
        ClockAuthority::Osc => osc_blue_bg,
    };
    let w = draw_badge(ui, x, auth_label, auth_bg);
    x += w + group_gap;

    // ── LINK group ──────────────────────────────────────────────────
    let link_bg = if sync.link_enabled {
        link_orange_bg
    } else {
        inactive_bg
    };
    let w = draw_badge(ui, x, "LINK", link_bg);
    x += w + item_gap;

    let (link_dot_color, link_status, link_text_color) = if !sync.link_enabled {
        (dot_inactive, "Off".to_string(), dimmed)
    } else if sync.link_peers > 0 {
        (
            dot_green,
            format!(
                "{} peer{}",
                sync.link_peers,
                if sync.link_peers == 1 { "" } else { "s" }
            ),
            white,
        )
    } else {
        (dot_yellow, "Listening".to_string(), dimmed)
    };

    draw_dot(ui, x, link_dot_color);
    x += dot_size + text_gap;
    ui.draw_text(x, text_y, &link_status, status_font as f32, link_text_color);
    let status_w = ui
        .measure_text_cached(&link_status, status_font, FontWeight::Medium)
        .x;
    x += status_w + group_gap;

    // ── CLK group ───────────────────────────────────────────────────
    let clk_bg = if sync.midi_clock_enabled {
        midi_purple_bg
    } else {
        inactive_bg
    };
    let w = draw_badge(ui, x, "CLK", clk_bg);
    x += w + item_gap;

    // Device name badge (always inactive bg, like the standard bar).
    let device_text = if sync.midi_clock_device_name.is_empty() {
        if sync.midi_clock_enabled { "MIDI" } else { "Select..." }
    } else {
        &sync.midi_clock_device_name
    };
    let w = draw_badge(ui, x, device_text, inactive_bg);
    x += w + item_gap;

    let (clk_dot_color, clk_status, clk_text_color) = if !sync.midi_clock_enabled {
        (dot_inactive, "Off".to_string(), dimmed)
    } else if sync.midi_clock_receiving {
        let pos = if sync.midi_clock_position_display.is_empty() {
            "Receiving".to_string()
        } else {
            sync.midi_clock_position_display.clone()
        };
        (dot_green, pos, white)
    } else {
        (dot_yellow, "Waiting".to_string(), dimmed)
    };

    draw_dot(ui, x, clk_dot_color);
    x += dot_size + text_gap;
    ui.draw_text(x, text_y, &clk_status, status_font as f32, clk_text_color);
}

/// Draw a numeric string digit-by-digit, each digit centered within a
/// fixed-width column. Non-digit chars (".", "-") use their natural width.
/// Returns the total rendered width.
fn draw_numeric_text(
    ui: &mut UIRenderer,
    x: f32,
    y: f32,
    text: &str,
    font_size: u16,
    color: [u8; 4],
) -> f32 {
    let digit_w = ui.measure_text_cached("0", font_size, FontWeight::Medium).x;
    let mut cur_x = x;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let s = ch.encode_utf8(&mut buf);
        if ch.is_ascii_digit() {
            // Center the digit within its fixed column so narrow digits
            // ("1") don't look left-shifted next to wide digits ("8").
            let actual = ui.measure_text_cached(s, font_size, FontWeight::Medium).x;
            let offset = (digit_w - actual) * 0.5;
            ui.draw_text(cur_x + offset, y, s, font_size as f32, color);
            cur_x += digit_w;
        } else {
            let actual = ui.measure_text_cached(s, font_size, FontWeight::Medium).x;
            ui.draw_text(cur_x, y, s, font_size as f32, color);
            cur_x += actual;
        }
    }
    cur_x - x
}

/// Draw the bottom-center exit button. Intentionally small so it cannot
/// be clicked accidentally during a performance — the cue HUD is the
/// primary visual, and the exit affordance is deliberate-only.
/// Caller must have already called `ui.begin_frame()`.
fn draw_exit_button(
    ui: &mut UIRenderer,
    btn_x: f32,
    btn_y: f32,
    btn_w: f32,
    btn_h: f32,
    hover: bool,
) {
    // Muted bg by default; brighter on hover so the user gets visual
    // confirmation they're aiming at it before pressing.
    let bg = if hover {
        [0.55, 0.10, 0.10, 1.0]
    } else {
        [0.20, 0.06, 0.06, 1.0]
    };
    ui.draw_rounded_rect(btn_x, btn_y, btn_w, btn_h, bg, 6.0);

    let label = "EXIT";
    let font_size_px: u16 = 12;
    let text_size = ui.measure_text_cached(label, font_size_px, FontWeight::Medium);
    let text_x = btn_x + (btn_w - text_size.x) * 0.5;
    let text_y = btn_y + (btn_h - text_size.y) * 0.5;
    let text_color = if hover {
        [255u8, 220u8, 220u8, 255u8]
    } else {
        [180u8, 140u8, 140u8, 255u8]
    };
    ui.draw_text(text_x, text_y, label, font_size_px as f32, text_color);
}

/// Draw the center column of the perform-mode HUD: NOW + NEXT + big
/// countdown number + locator countdown bar + bar.beat readout, plus the
/// slim status row at the bottom (BPM, transport, Ableton state — no
/// BEAT, since bar.beat replaces it).
///
/// Caller must have already called `ui.begin_frame()`.
#[allow(clippy::too_many_arguments)]
fn draw_cue_hud(
    ui: &mut UIRenderer,
    lw: f32,
    lh: f32,
    current_name: &str,
    next_name: &str,
    countdown: Option<&cue::CountdownDisplay>,
    section_progress: Option<f64>,
    bar_beat: &cue::BarBeatDisplay,
    now_section_tracks: &[&str],
    next_section_tracks: &[&str],
    bpm: f64,
    is_playing: bool,
    ableton_connected: bool,
    cues_empty: bool,
) {
    // Color palette.
    let dim = [140u8, 140u8, 145u8, 255u8];
    let white = [240u8, 240u8, 240u8, 255u8];
    let bar_bg = [0.10, 0.10, 0.12, 1.0];
    let warn = [240u8, 200u8, 60u8, 255u8];

    // Traffic-light palette:
    //   Green  = NOW (safe, this is playing)
    //   Yellow→Red = NEXT countdown (urgency ramp as bars approach 0)
    let now_green = [60u8, 220u8, 90u8, 255u8];
    let now_green_dim = [50u8, 170u8, 70u8, 255u8];

    // Countdown urgency: lerp yellow→red based on bars remaining.
    // >4 bars = pure yellow, <2 bars = pure red, linear blend between.
    let bars_remaining = countdown
        .map(|cd| cd.number.parse::<f32>().unwrap_or(99.0))
        .unwrap_or(99.0);
    let urgency = ((4.0 - bars_remaining) / 2.0).clamp(0.0, 1.0); // 0=yellow, 1=red
    let next_color = [
        (240.0 + (255.0 - 240.0) * urgency) as u8,  // R: 240→255
        (200.0 - 140.0 * urgency) as u8,              // G: 200→60
        (60.0 - 30.0 * urgency) as u8,                // B: 60→30
        255u8,
    ];
    let next_color_f = [
        next_color[0] as f32 / 255.0,
        next_color[1] as f32 / 255.0,
        next_color[2] as f32 / 255.0,
        1.0,
    ];

    // ── NOW ────────────────────────────────────────────────────────
    let label_now = "NOW";
    let label_size: u16 = 18;
    let now_size: u16 = 64;
    let track_size: u16 = 20;
    let track_line_h = track_size as f32 + 6.0;
    const MAX_TRACKS: usize = 5;
    let label_dim = ui.measure_text_cached(label_now, label_size, FontWeight::Medium);
    let now_dim = ui.measure_text_cached(current_name, now_size, FontWeight::Medium);
    let now_top = lh * 0.06;
    ui.draw_text(
        (lw - label_dim.x) * 0.5,
        now_top,
        label_now,
        label_size as f32,
        now_green_dim,
    );
    ui.draw_text(
        (lw - now_dim.x) * 0.5,
        now_top + label_size as f32 + 8.0,
        current_name,
        now_size as f32,
        now_green,
    );

    // ── NOW track list (vertical, centered, under the NOW name) ────
    let now_track_top = now_top + label_size as f32 + 8.0 + now_size as f32 + 12.0;
    if !now_section_tracks.is_empty() {
        let mut ny = now_track_top;
        for name in now_section_tracks.iter().take(MAX_TRACKS) {
            let w = ui.measure_text_cached(name, track_size, FontWeight::Medium).x;
            ui.draw_text((lw - w) * 0.5, ny, name, track_size as f32, now_green_dim);
            ny += track_line_h;
        }
    }

    // ── NEXT ───────────────────────────────────────────────────────
    let label_next = "NEXT";
    let next_size: u16 = 64;
    let countdown_size: u16 = 72;
    let label_next_dim = ui.measure_text_cached(label_next, label_size, FontWeight::Medium);
    let next_name_dim = ui.measure_text_cached(next_name, next_size, FontWeight::Medium);
    // Position NEXT below the 5-track slot so it never overlaps NOW.
    let next_top = (now_track_top + MAX_TRACKS as f32 * track_line_h + 16.0)
        .max(lh * 0.32);
    ui.draw_text(
        (lw - label_next_dim.x) * 0.5,
        next_top,
        label_next,
        label_size as f32,
        next_color,
    );
    ui.draw_text(
        (lw - next_name_dim.x) * 0.5,
        next_top + label_size as f32 + 8.0,
        next_name,
        next_size as f32,
        next_color,
    );

    // ── NEXT track list (between name and countdown) ──────────────
    let mut next_content_y = next_top + label_size as f32 + 8.0 + next_size as f32;
    if !next_section_tracks.is_empty() {
        next_content_y += 10.0;
        for name in next_section_tracks.iter().take(MAX_TRACKS) {
            let w = ui.measure_text_cached(name, track_size, FontWeight::Medium).x;
            ui.draw_text((lw - w) * 0.5, next_content_y, name, track_size as f32, next_color);
            next_content_y += track_line_h;
        }
    }

    // ── Countdown number (fixed-column digits, anchored center axis) ─
    // Anchored from the bottom so it never jumps when the track count
    // changes — the progress bar and bar.beat readout below it are also
    // bottom-anchored, keeping the whole lower HUD stable.
    let countdown_y = (lh - 300.0).max(next_content_y + 16.0);
    if let Some(cd) = countdown {
        let gap = 24.0_f32;
        let center_x = lw * 0.5;
        let number_right = center_x - gap * 0.5;
        let unit_left = center_x + gap * 0.5;

        let num_w = numeric_text_width(ui, &cd.number, countdown_size);
        draw_numeric_text(
            ui,
            number_right - num_w,
            countdown_y,
            &cd.number,
            countdown_size,
            next_color,
        );
        ui.draw_text(
            unit_left,
            countdown_y,
            &cd.unit,
            countdown_size as f32,
            next_color,
        );
    } else {
        // No next cue — show a centered em-dash placeholder.
        let placeholder = "—";
        let dim_size = ui.measure_text_cached(placeholder, countdown_size, FontWeight::Medium);
        ui.draw_text(
            (lw - dim_size.x) * 0.5,
            countdown_y,
            placeholder,
            countdown_size as f32,
            next_color,
        );
    }

    // ── Locator countdown bar — directly under the countdown number ─
    //
    // Hidden when there's no next cue (we're past the last locator) so
    // the absence is meaningful. When present, fills left→right as the
    // playhead progresses through the current section.
    if let Some(p) = section_progress {
        let bar_w = (lw * 0.32).clamp(220.0, 480.0);
        let bar_h = 14.0_f32;
        let bar_x = (lw - bar_w) * 0.5;
        let bar_y = countdown_y + countdown_size as f32 + 18.0;
        // Background
        ui.draw_rounded_rect(bar_x, bar_y, bar_w, bar_h, bar_bg, bar_h * 0.5);
        // Fill — width proportional to progress through section
        let fill_w = (bar_w * p as f32).max(0.0);
        if fill_w > 1.0 {
            ui.draw_rounded_rect(bar_x, bar_y, fill_w, bar_h, next_color_f, bar_h * 0.5);
        }
    }

    // ── Slim status row, raised away from the exit button ───────────
    //
    // Just BPM + transport + Ableton state. 3 fixed cells across.
    let status_size: u16 = 16;
    // Exit button bottom-anchored at lh - 36 - 24. Push status row well
    // above it so the two never feel coupled.
    let status_y = lh - 36.0 - 24.0 - 64.0;

    // ── BAR.BEAT.SIXTEENTH readout (Ableton transport style) ────────
    //
    // Sits directly above the status row's PLAYING line so the "where am
    // I in the song" indicator anchors to the bottom rather than floating
    // in the middle of the HUD.
    let bb_size: u16 = 36;
    let bb_y = status_y - bb_size as f32 - 16.0;
    {
        let dot = " . ";
        let dot_w = ui.measure_text_cached(dot, bb_size, FontWeight::Medium).x;
        let bar_w = numeric_text_width(ui, &bar_beat.bar, bb_size);
        let beat_w = numeric_text_width(ui, &bar_beat.beat, bb_size);
        let six_w = numeric_text_width(ui, &bar_beat.sixteenth, bb_size);
        let total_w = bar_w + dot_w + beat_w + dot_w + six_w;
        let start_x = (lw - total_w) * 0.5;

        let mut x = start_x;
        draw_numeric_text(ui, x, bb_y, &bar_beat.bar, bb_size, white);
        x += bar_w;
        ui.draw_text(x, bb_y, dot, bb_size as f32, dim);
        x += dot_w;
        draw_numeric_text(ui, x, bb_y, &bar_beat.beat, bb_size, white);
        x += beat_w;
        ui.draw_text(x, bb_y, dot, bb_size as f32, dim);
        x += dot_w;
        draw_numeric_text(ui, x, bb_y, &bar_beat.sixteenth, bb_size, white);
    }

    let bpm_value = format!("{:.1}", bpm);
    let play_value = if is_playing { "▶ PLAYING" } else { "■ STOPPED" };
    let conn_value = if !ableton_connected {
        "DISCONNECTED"
    } else if cues_empty {
        "NO CUES"
    } else {
        "OK"
    };
    let conn_color = if !ableton_connected || cues_empty {
        warn
    } else {
        dim
    };

    let n = 3.0_f32;
    let outer_pad = 64.0_f32;
    let usable = lw - outer_pad * 2.0;
    let cell_w = usable / n;
    let cell_centers: [f32; 3] = [
        outer_pad + cell_w * 0.5,
        outer_pad + cell_w * 1.5,
        outer_pad + cell_w * 2.5,
    ];

    let label_value_gap = 8.0_f32;

    let draw_numeric_cell =
        |ui: &mut UIRenderer, center_x: f32, label: &str, value: &str| {
            let label_w = ui
                .measure_text_cached(label, status_size, FontWeight::Medium)
                .x;
            let value_w = numeric_text_width(ui, value, status_size);
            let total_w = label_w + label_value_gap + value_w;
            let start_x = center_x - total_w * 0.5;
            ui.draw_text(start_x, status_y, label, status_size as f32, dim);
            draw_numeric_text(
                ui,
                start_x + label_w + label_value_gap,
                status_y,
                value,
                status_size,
                dim,
            );
        };

    let draw_slotted_cell =
        |ui: &mut UIRenderer,
         center_x: f32,
         label: &str,
         value: &str,
         max_value: &str,
         color: [u8; 4]| {
            let label_w = if label.is_empty() {
                0.0
            } else {
                ui.measure_text_cached(label, status_size, FontWeight::Medium)
                    .x
            };
            let slot_w = ui
                .measure_text_cached(max_value, status_size, FontWeight::Medium)
                .x;
            let value_w = ui
                .measure_text_cached(value, status_size, FontWeight::Medium)
                .x;
            let gap = if label.is_empty() { 0.0 } else { label_value_gap };
            let total_w = label_w + gap + slot_w;
            let start_x = center_x - total_w * 0.5;
            if !label.is_empty() {
                ui.draw_text(start_x, status_y, label, status_size as f32, dim);
            }
            let value_x = start_x + label_w + gap + (slot_w - value_w) * 0.5;
            ui.draw_text(value_x, status_y, value, status_size as f32, color);
        };

    draw_numeric_cell(ui, cell_centers[0], "BPM", &bpm_value);
    draw_slotted_cell(
        ui,
        cell_centers[1],
        "",
        play_value,
        "▶ PLAYING",
        dim,
    );
    draw_slotted_cell(
        ui,
        cell_centers[2],
        "ABLETON",
        conn_value,
        "DISCONNECTED",
        conn_color,
    );
}

/// Draw the macros bar-graph column on the LEFT side of the HUD.
///
/// Each entry is a label (the macro's Ableton name) above a horizontal
/// progress bar showing the macro's current 0..=1 value. The column is
/// fixed-width and left-anchored with a small inner pad. Hidden by the
/// caller when the snapshot is empty (no macros mapped).
///
/// Caller must have already called `ui.begin_frame()`.
fn draw_macros_column(
    ui: &mut UIRenderer,
    lh: f32,
    macros: &[perform_macros::MacroDisplay],
) {
    let dim = [140u8, 140u8, 145u8, 255u8];
    let white = [240u8, 240u8, 240u8, 255u8];
    // Ableton purple — matches the macro accent used elsewhere in the
    // main UI (see ABL_BADGE_C32 / ABL_TRIM_BAR_C32 in manifold-ui::color).
    let macro_fill = [140.0 / 255.0, 80.0 / 255.0, 200.0 / 255.0, 1.0];
    let bar_bg = [0.10, 0.10, 0.12, 1.0];

    let label_size: u16 = 18;
    let name_size: u16 = 16;
    let bar_w = 220.0_f32;
    let bar_h = 10.0_f32;
    let line_h: f32 = (name_size as f32) + bar_h + 14.0;
    let left_pad = 48.0_f32;

    // Header
    let header = "MACROS";
    let header_y = lh * 0.14;
    ui.draw_text(left_pad, header_y, header, label_size as f32, dim);

    let block_y = header_y + label_size as f32 + 18.0;

    for (i, m) in macros.iter().enumerate() {
        let row_top = block_y + line_h * i as f32;
        // Name (left-aligned)
        ui.draw_text(left_pad, row_top, &m.name, name_size as f32, white);
        // Bar background
        let bar_y = row_top + name_size as f32 + 6.0;
        ui.draw_rounded_rect(left_pad, bar_y, bar_w, bar_h, bar_bg, bar_h * 0.5);
        // Bar fill
        let fill_w = (bar_w * m.value.clamp(0.0, 1.0)).max(0.0);
        if fill_w > 1.0 {
            ui.draw_rounded_rect(left_pad, bar_y, fill_w, bar_h, macro_fill, bar_h * 0.5);
        }
    }
}
