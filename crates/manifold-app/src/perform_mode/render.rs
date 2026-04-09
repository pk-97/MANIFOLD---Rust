//! Perform-mode rendering — minimal main-window draw path.

use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::FontWeight;

use crate::app::Application;
use crate::perform_mode::{cue, tracks};

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

        // Exit button — bottom-center, smaller than before so the cue HUD
        // owns the screen real estate. Always reachable.
        let btn_w = 280.0_f32;
        let btn_h = 64.0_f32;
        let btn_x = (lw - btn_w) * 0.5;
        let btn_y = lh - btn_h - 32.0;
        self.perform.exit_button_rect =
            manifold_ui::node::Rect::new(btn_x, btn_y, btn_w, btn_h);

        // Snapshot HUD inputs from content_state + persistent ui_root cache.
        let current_beat = self.content_state.current_beat.0;
        let bpm = self.content_state.bpm;
        let beats_per_bar = self.content_state.time_signature_numerator.max(1) as u32;
        let is_playing = self.content_state.is_playing;
        let ableton_connected = self.content_state.ableton_connected;
        // Cue points come from the persistent ui_root.ableton_session cache,
        // not content_state — content_state.ableton_session is edge-triggered
        // (Some only on the frame the bridge published a change).
        let cue_points: Vec<manifold_playback::ableton_bridge::CuePoint> = self
            .ui_root
            .ableton_session
            .as_ref()
            .map(|s| s.cue_points.clone())
            .unwrap_or_default();
        let analysis = cue::analyze(&cue_points, current_beat);
        let current_name = analysis
            .current
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "—".to_string());
        let next_name = analysis
            .next
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "—".to_string());
        let countdown = analysis
            .beats_to_next
            .map(|b| cue::format_countdown(b, beats_per_bar));

        // PLAY-group tracks: snapshot from the persistent ui_root cache.
        // We compute (name, is_playing) up-front so the renderer doesn't
        // need to hold a borrow on `self.ui_root` while it borrows
        // `self.ui_renderer`.
        let play_tracks: Vec<(String, bool)> = self
            .ui_root
            .ableton_session
            .as_ref()
            .and_then(|s| s.play_group.as_ref())
            .map(|g| {
                g.tracks
                    .iter()
                    .map(|t| (t.name.clone(), tracks::is_playing(t, current_beat)))
                    .collect()
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
                &current_name,
                &next_name,
                countdown.as_ref(),
                bpm,
                current_beat,
                is_playing,
                ableton_connected,
                cue_points.is_empty(),
            );

            if !play_tracks.is_empty() {
                draw_play_group_column(ui, lw, lh, &play_tracks);
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

/// Draw the PLAY-group track column on the right side of the HUD.
///
/// Each track is rendered with a small accent dot (when playing) plus
/// the track name. Playing tracks render in white at full opacity;
/// non-playing tracks render dim. The column is fixed-width and
/// right-anchored to the screen edge with a small inner pad.
///
/// Caller must have already called `ui.begin_frame()`.
fn draw_play_group_column(
    ui: &mut UIRenderer,
    lw: f32,
    lh: f32,
    tracks: &[(String, bool)],
) {
    let dim = [110u8, 110u8, 115u8, 255u8];
    let white = [240u8, 240u8, 240u8, 255u8];
    let accent_f = [1.0, 0.35, 0.27, 1.0]; // dot fill
    let accent_text = [255u8, 90u8, 70u8, 255u8];

    let label_size: u16 = 18;
    let track_size: u16 = 18;
    let line_h: f32 = (track_size as f32) + 8.0;
    let dot_d: f32 = 8.0;
    let dot_text_gap: f32 = 10.0;
    let right_pad: f32 = 32.0;

    // Column width: enough for the longest track name + dot + padding.
    // Pre-measure with the widest realistic name to keep the column anchor
    // independent of which tracks happen to be playing this frame.
    let max_name_w = tracks
        .iter()
        .map(|(n, _)| {
            ui.measure_text_cached(n, track_size, FontWeight::Medium)
                .x
        })
        .fold(0.0_f32, f32::max);
    let col_w = max_name_w + dot_d + dot_text_gap;
    let col_x = lw - right_pad - col_w;
    // Vertically center the block within the upper portion of the screen
    // so it lines up roughly with the cue HUD's "NOW" / "NEXT" stack.
    let total_h = line_h * tracks.len() as f32;
    let label_text = "PLAY";
    let label_dim = ui.measure_text_cached(label_text, label_size, FontWeight::Medium);
    let label_y = lh * 0.18;
    // Right-align the label to the column right edge for visual symmetry.
    let label_x = col_x + col_w - label_dim.x;
    ui.draw_text(label_x, label_y, label_text, label_size as f32, dim);

    let block_y = label_y + label_size as f32 + 12.0;
    // Cap block height — if there are too many tracks to fit, just let
    // them extend toward the status row. The exit button has its own
    // safe zone below the status row.
    let max_block_h = lh * 0.70 - block_y;
    let _ = max_block_h; // (informational; we currently don't truncate)
    let _ = total_h;

    for (i, (name, playing)) in tracks.iter().enumerate() {
        let y = block_y + line_h * i as f32;
        let text_color = if *playing { white } else { dim };
        // Dot — left-anchored at col_x
        if *playing {
            // Center dot vertically against the text x-height (≈ 60% of line_h).
            let dot_y = y + (track_size as f32 - dot_d) * 0.5 + 2.0;
            ui.draw_rounded_rect(col_x, dot_y, dot_d, dot_d, accent_f, dot_d * 0.5);
        }
        let text_x = col_x + dot_d + dot_text_gap;
        ui.draw_text(text_x, y, name, track_size as f32, text_color);
    }
    let _ = accent_text; // reserved for future use
}

/// Draw the bottom-center "EXIT PERFORMANCE MODE" button.
/// Caller must have already called `ui.begin_frame()`.
fn draw_exit_button(
    ui: &mut UIRenderer,
    btn_x: f32,
    btn_y: f32,
    btn_w: f32,
    btn_h: f32,
    hover: bool,
) {
    let bg = if hover {
        [0.85, 0.18, 0.18, 1.0]
    } else {
        [0.62, 0.12, 0.12, 1.0]
    };
    ui.draw_rounded_rect(btn_x, btn_y, btn_w, btn_h, bg, 10.0);

    let label = "EXIT PERFORMANCE MODE";
    let font_size_px: u16 = 16;
    let text_size = ui.measure_text_cached(label, font_size_px, FontWeight::Medium);
    let text_x = btn_x + (btn_w - text_size.x) * 0.5;
    let text_y = btn_y + (btn_h - text_size.y) * 0.5;
    ui.draw_text(
        text_x,
        text_y,
        label,
        font_size_px as f32,
        [255, 255, 255, 255],
    );
}

/// Draw the cue HUD: top-third "NOW", middle-third "NEXT" + countdown,
/// bottom-status row above the exit button (BPM, beat, connection state).
/// Caller must have already called `ui.begin_frame()`.
///
/// The countdown is laid out around a **fixed center axis** (`lw / 2`):
/// the number is right-aligned to the left of the axis, the unit is
/// left-aligned to the right. This eliminates the per-frame position
/// jitter that comes from re-centering a string whose width changes when
/// digits roll over. Only the (invisible) left edge of the number ever
/// moves; the right edge of the number and the left edge of the unit
/// stay locked.
#[allow(clippy::too_many_arguments)]
fn draw_cue_hud(
    ui: &mut UIRenderer,
    lw: f32,
    lh: f32,
    current_name: &str,
    next_name: &str,
    countdown: Option<&cue::CountdownDisplay>,
    bpm: f64,
    current_beat: f64,
    is_playing: bool,
    ableton_connected: bool,
    cues_empty: bool,
) {
    // Color palette.
    let dim = [140u8, 140u8, 145u8, 255u8];
    let white = [240u8, 240u8, 240u8, 255u8];
    let accent = [255u8, 90u8, 70u8, 255u8];
    let warn = [240u8, 200u8, 60u8, 255u8];

    // ── NOW ────────────────────────────────────────────────────────
    let label_now = "NOW";
    let label_size: u16 = 18;
    let now_size: u16 = 64;
    let label_dim = ui.measure_text_cached(label_now, label_size, FontWeight::Medium);
    let now_dim = ui.measure_text_cached(current_name, now_size, FontWeight::Medium);
    let now_top = lh * 0.18;
    ui.draw_text(
        (lw - label_dim.x) * 0.5,
        now_top,
        label_now,
        label_size as f32,
        dim,
    );
    ui.draw_text(
        (lw - now_dim.x) * 0.5,
        now_top + label_size as f32 + 8.0,
        current_name,
        now_size as f32,
        white,
    );

    // ── NEXT ───────────────────────────────────────────────────────
    let label_next = "NEXT";
    let next_size: u16 = 40;
    let countdown_size: u16 = 96;
    let label_next_dim = ui.measure_text_cached(label_next, label_size, FontWeight::Medium);
    let next_name_dim = ui.measure_text_cached(next_name, next_size, FontWeight::Medium);
    let next_top = lh * 0.46;
    ui.draw_text(
        (lw - label_next_dim.x) * 0.5,
        next_top,
        label_next,
        label_size as f32,
        dim,
    );
    ui.draw_text(
        (lw - next_name_dim.x) * 0.5,
        next_top + label_size as f32 + 8.0,
        next_name,
        next_size as f32,
        white,
    );

    // ── Countdown — anchored on a fixed center axis ─────────────────
    //
    // The number is rendered digit-by-digit in fixed columns (constant
    // width regardless of which digits), then right-aligned to a fixed
    // anchor at `lw/2 - gap/2`. The unit is left-aligned at `lw/2 + gap/2`.
    // Both anchors are immovable, and because the number's pixel width is
    // now constant, even the LEFT edge of the number stays put.
    let countdown_y = next_top + label_size as f32 + next_size as f32 + 16.0;
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
            accent,
        );
        ui.draw_text(
            unit_left,
            countdown_y,
            &cd.unit,
            countdown_size as f32,
            accent,
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
            accent,
        );
    }

    // ── Bottom status row (above exit button) ───────────────────────
    //
    // Layout: 4 fixed cells, each rendering a tight (label + value) pair
    // centered on the cell's center. Numeric values use fixed-column
    // digits for constant width. Non-numeric values reserve their
    // longest-possible-string width as a fixed slot so transitions
    // (PLAYING ↔ STOPPED, OK ↔ DISCONNECTED) don't shift the cell.
    let status_size: u16 = 16;
    let status_y = lh - 64.0 - 32.0 - 28.0;
    let bpm_value = format!("{:.1}", bpm);
    let beat_value = format!("{:.1}", current_beat);
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

    // Cell layout: 4 evenly-spaced cells across the row.
    let n = 4.0_f32;
    let outer_pad = 48.0_f32;
    let usable = lw - outer_pad * 2.0;
    let cell_w = usable / n;
    let cell_centers: [f32; 4] = [
        outer_pad + cell_w * 0.5,
        outer_pad + cell_w * 1.5,
        outer_pad + cell_w * 2.5,
        outer_pad + cell_w * 3.5,
    ];

    let label_value_gap = 8.0_f32;

    // Helper: draw a (label, numeric_value) pair centered on a cell center.
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

    // Helper: draw a value that may switch between several strings,
    // reserving a fixed slot equal to the widest possible content. The
    // value is centered within the slot.
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
            // Center the actual value within the reserved slot.
            let value_x = start_x + label_w + gap + (slot_w - value_w) * 0.5;
            ui.draw_text(value_x, status_y, value, status_size as f32, color);
        };

    // Cell 0: BPM <numeric>
    draw_numeric_cell(ui, cell_centers[0], "BPM", &bpm_value);
    // Cell 1: BEAT <numeric>
    draw_numeric_cell(ui, cell_centers[1], "BEAT", &beat_value);
    // Cell 2: PLAYING / STOPPED (max width = "▶ PLAYING")
    draw_slotted_cell(
        ui,
        cell_centers[2],
        "",
        play_value,
        "▶ PLAYING",
        dim,
    );
    // Cell 3: ABLETON <state> (max width = "DISCONNECTED")
    draw_slotted_cell(
        ui,
        cell_centers[3],
        "ABLETON",
        conn_value,
        "DISCONNECTED",
        conn_color,
    );
}
