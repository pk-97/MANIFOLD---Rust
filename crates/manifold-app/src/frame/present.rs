//! Frame present stage: the multi-window present pass
//! (`present_all_windows`) and the cached-offscreen re-present, plus the
//! BUG-060 dump + scope-readout helpers. Moved verbatim from app_render.rs
//! (UI_FUNNEL_DECOMPOSITION P-F1, pure move). The drain/events/sync/push
//! sibling stages remain inline in tick_and_render (parked — semantic).

use crate::app::Application;

static BUG060_DUMP_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Dump cadence: `MANIFOLD_BUG060_DUMP=<N>` dumps every N dirty-present frames
/// (minimum 2); any other non-empty value (e.g. `=1`) means the default of 30.
/// Unset → `None`, and the dump code is never reached.
fn bug060_dump_every() -> Option<u64> {
    static EVERY: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| {
        std::env::var("MANIFOLD_BUG060_DUMP")
            .ok()
            .map(|v| v.parse::<u64>().ok().filter(|&e| e >= 2).unwrap_or(30))
    })
}

/// Read `tex` (Bgra8Unorm) back and overwrite `path` with an opaque RGBA8 PNG.
/// Alpha is forced to 255 so viewers don't render the atlas's cleared-to-zero
/// regions as white; B/R are swapped for the PNG only.
fn bug060_dump_png(
    device: &manifold_gpu::GpuDevice,
    tex: &manifold_gpu::GpuTexture,
    path: &str,
) {
    let (w, h) = (tex.width, tex.height);
    if w == 0 || h == 0 {
        return;
    }
    let bytes_per_row = w * 4;
    let total = u64::from(h) * u64::from(bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("bug060-dump");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let Some(ptr) = buf.mapped_ptr() else {
        eprintln!("[BUG-060] {path}: readback buffer not mapped");
        return;
    };
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ptr, total as usize) };
    let mut rgba = bytes.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[BUG-060] {path}: {e}");
            return;
        }
    };
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    match encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&rgba))
    {
        Ok(()) => eprintln!("[BUG-060] wrote {path} ({w}x{h})"),
        Err(e) => eprintln!("[BUG-060] {path}: {e}"),
    }
}

/// Format the audio scope's hover readout: frequency (kHz above 1 kHz, else Hz)
/// and the raw level in dB, e.g. `4.17 kHz   -17.9 dB`.
pub(crate) fn format_scope_readout(freq: f32, db: f32) -> String {
    let f = if freq >= 1000.0 {
        format!("{:.2} kHz", freq / 1000.0)
    } else {
        format!("{freq:.0} Hz")
    };
    format!("{f}   {db:.1} dB")
}

/// Seed text for the inline `Table` cell editor — compact but lossless enough
/// to round-trip: integers without a decimal point, fractionals to four places
/// with trailing zeros trimmed.
pub(crate) fn fmt_table_cell_seed(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1.0e7 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

impl Application {
    pub(crate) fn present_all_windows(&mut self, front_index: usize) {
        let Some(gpu) = &self.gpu else { return };

        // UI frame profiler cursor (no-op unless MANIFOLD_UI_FRAME_PROFILE=1).
        let mut pseg = std::time::Instant::now();

        // ── Panel cache update: ensure the atlas is sized for the current
        // surface. `render_dirty_panels` itself now runs inside
        // `ui_frame::composite_main_ui_frame` (P1, D3), called below from the
        // non-fast-path branch. That's behavior-preserving, not just
        // convenient: `self.ws.offscreen_dirty` is set true whenever any
        // panel node is dirty (`has_dirty_in_range(0, panel_end)`, this
        // function's caller), so `render_dirty_panels` is already a no-op on
        // every frame the fast path below takes — deferring its call site
        // changes no pixel it produces.
        let scale = self.scale_factor;
        if let (Some(cm), Some(_ui)) = (&mut self.ui_cache_manager, &self.ui_renderer) {
            // Compute logical surface dimensions
            let (surface_w, surface_h) = self
                .primary_window_id
                .and_then(|id| self.window_registry.get(&id))
                .and_then(|ws| ws.surface.as_ref())
                .map(|s| (s.width, s.height))
                .unwrap_or((1, 1));
            let logical_w = (surface_w as f64 / scale) as u32;
            let logical_h = (surface_h as f64 / scale) as u32;
            cm.set_scale_factor(scale);
            cm.ensure_atlas(&gpu.device, logical_w, logical_h);
        }
        self.ui_profile.add("present.panel_cache", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Render target: offscreen texture ──
        // All passes render to an offscreen texture. The drawable is acquired
        // late (just before present) to minimize time blocking on WindowServer
        // IPC during Direct Display synchronization on external monitors.
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

        let Some(offscreen) = &self.ws.ui_offscreen else {
            return;
        };
        // Ensure offscreen matches surface (may be stale after resize race).
        if offscreen.width != surface_w || offscreen.height != surface_h {
            return;
        }

        let logical_w = (surface_w as f64 / scale) as u32;
        let logical_h = (surface_h as f64 / scale) as u32;
        let sf = scale as f32;

        // ── Fast path: nothing visual changed — re-blit cached offscreen.
        // Must still present every callback to maintain consistent cadence.
        // ProMotion adapts refresh rate based on observed frame delivery;
        // skipping presents causes it to drop from 120Hz to 60Hz, producing
        // an 8/16ms nextDrawable bounce when it oscillates back.
        if !self.ws.offscreen_dirty {
            if self.ws.surface_resized_this_frame {
                self.ws.surface_resized_this_frame = false;
                return;
            }
            self.represent_cached_offscreen(window_id, &mut pseg);
            return;
        }

        // ── Admission control: the offscreen DOES need a redraw, but the
        // GPU is badly behind on retiring already-encoded UI work. Encoding
        // yet another frame's ring-owner passes (layer bitmap, clip content/
        // thumb, UI renderer) would just mean more `guard_slot` callers
        // blocking mid-encode for up to `WAIT_TIMEOUT` — up to 50ms of UI
        // stall — once the ring wraps into an unretired slot. Skip this
        // redraw instead: re-present the still-valid cached offscreen (same
        // pixels as last frame) and leave `offscreen_dirty` set so the
        // pending redraw runs the moment the GPU catches up. Gated on the
        // resize/size checks above already having passed (surface not mid-
        // resize, offscreen dims match) so this never re-presents a stale-
        // sized frame.
        if let Some(lag) = self
            .ui_frame_fence
            .as_ref()
            .map(|f| f.lag())
            .filter(|&lag| lag > 3)
        {
            self.ui_frame_fence_skip_events += 1;
            let n = self.ui_frame_fence_skip_events;
            if n <= 3 || n.is_multiple_of(256) {
                log::info!(
                    "[frame-fence] UI redraw skipped, GPU {lag} frames behind — \
                     re-presenting cached frame"
                );
            }
            self.represent_cached_offscreen(window_id, &mut pseg);
            return;
        }
        self.ws.offscreen_dirty = false;

        // Reset overlay TextRenderer pool index
        if let Some(ui) = &mut self.ui_renderer {
            ui.begin_frame();
        }

        // ── Build the frame: dirty-panel atlas render + clear-to-black +
        // full-atlas blit + optional video-band blit — the composite seam
        // shared with the headless harness (`ui_frame::composite_main_ui_
        // frame`, P1, D3). Pass 4/5 below (timeline tracks, overlays) and
        // the drawable tail stay here unchanged, on their own encoder
        // created after this call returns — composite_main_ui_frame owns
        // and commits its own encoder internally (see its module doc
        // deviation #3 for why it takes the pipeline/sampler/scale params
        // it does).
        pseg = std::time::Instant::now();
        #[cfg(target_os = "macos")]
        let compositor_tex = self.ui_preview_textures[front_index].as_ref();
        #[cfg(not(target_os = "macos"))]
        let compositor_tex: Option<&manifold_gpu::GpuTexture> = None;
        let video_source_dims = self
            .content_pipeline_output
            .as_ref()
            .map(|p| {
                let (w, h) = p.get_dimensions();
                (w as f32, h as f32)
            })
            .unwrap_or((1920.0, 1080.0));
        if let (
            Some(cm),
            Some(ui),
            Some(atlas_pipeline),
            Some(atlas_sampler),
            Some(blit_pipeline),
            Some(blit_sampler),
        ) = (
            &mut self.ui_cache_manager,
            &mut self.ui_renderer,
            &self.atlas_pipeline,
            &self.atlas_sampler,
            &self.blit_pipeline,
            &self.blit_sampler,
        ) {
            crate::ui_frame::composite_main_ui_frame(
                &gpu.device,
                ui,
                cm,
                &mut self.ws.ui_root,
                offscreen,
                atlas_pipeline,
                atlas_sampler,
                blit_pipeline,
                blit_sampler,
                scale,
                compositor_tex,
                video_source_dims,
            );
        }
        self.ui_profile.add("present.clear_atlas_compositor", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Passes 4a→5 + VQT + overlay dirty-clear — the shared seam
        // (`ui_frame::render_main_ui_passes`, `HARNESS_FIDELITY_INVARIANT_
        // PROPOSAL.md` §4 step 2), also called by the headless harness
        // (`ui_snapshot/render.rs::render_ui_to_png`, `script.rs`'s
        // `Runner`). It owns its own encoder (created and committed
        // internally, mirroring `composite_main_ui_frame`) and pass order —
        // everything from here through the seam call below is INPUT
        // RESOLUTION: drag-adjusted clip bodies, thumbnail atlas + quads,
        // timeline overlays, automation lanes, scope cursor — kept here
        // because it's live-only/caller-side state (§3's caller test); the
        // seam itself decides pass order and per-pass render-call choice
        // and is never re-sequenced or re-implemented by any caller.

        // Pass 4b: GPU clip bodies — rounded gradient tiles with a lift-on-select
        // shadow, in their own UIRenderer prepare/render cycle (reusing the shared
        // SDF rect pipeline). Emitted from the viewport's visible-clip list, so
        // only on-screen clips cost anything.
        // Resolved only when the seam call below will actually run
        // (module doc deviation #9, `ui_frame.rs`): `self.ui_renderer`,
        // `self.blit_pipeline`/`self.blit_sampler`, and the GPU renderers
        // below are all `Some`/`None` together (set together at GPU init,
        // `app.rs` :1865-1993; cleared together at teardown, :2888-2893),
        // so this single bool gate reproduces the exact old per-pass gating.
        if self.ui_renderer.is_some() {
            self.ws
                .ui_root
                .viewport
                .visible_clip_rects(&mut self.clip_rect_scratch);
            // Cleared HERE, unconditionally, not only inside the has-clips
            // branch below: the seam now reads `clip_body_scratch`
            // unconditionally every frame (it's `MainUiPassInputs::
            // clip_bodies`, resolved caller-side once per frame, no longer
            // gated by the same `if` that populates it) — pre-extraction the
            // clear lived inside that `if`, which was safe only because
            // emission was co-located with it (a false condition skipped
            // both). Un-clearing on a no-clips frame would leave the LAST
            // frame's bodies in the buffer for the seam to render as ghost
            // clips over an empty view — moving the clear up here keeps that
            // failure mode impossible regardless of how the gate below
            // evaluates.
            self.clip_body_scratch.clear();
            // While an audio file is being dragged in from
            // Finder, show a full-length ghost clip at the lane/beat it
            // would land on — the same targeting the DroppedFile arm in
            // app.rs resolves, computed independently here (read-only
            // geometry, deliberately not shared with that gate-critical
            // code so this cosmetic addition can't regress it). Deferred:
            // a "New lane: <filename>" floating label for the non-audio-lane
            // case — no existing floating-text-over-viewport primitive to
            // reuse, and inventing one wasn't in scope for this pass.
            let ghost_body = self.drag_tracker.first_hovered_audio_seconds().and_then(|source_secs| {
                let pos = self.drag_tracker.drop_position().unwrap_or(self.cursor_pos);
                let vp = &self.ws.ui_root.viewport;
                let in_tracks = vp.get_tracks_rect().contains(pos);
                if !in_tracks {
                    return None;
                }
                let layer_index = vp.layer_at_y(pos.y)?;
                let layer = self.local_project.timeline.layers.get(layer_index)?;
                if !layer.is_audio() {
                    return None;
                }
                let start_beat = vp.pixel_to_beat(pos.x).as_f32().max(0.0);
                let spb = manifold_core::tempo::TempoMapConverter::seconds_per_beat_from_bpm(
                    self.local_project.settings.bpm.0,
                );
                let duration_beats =
                    if spb > 0.0 { source_secs.as_f32() / spb } else { 0.0 };
                Some(manifold_renderer::clip_draw::ClipBody {
                    rect: manifold_ui::node::Rect::new(
                        vp.beat_to_pixel(manifold_core::Beats::from_f32(start_beat)),
                        vp.track_y(layer_index),
                        vp.beat_duration_to_width(duration_beats),
                        vp.track_height(layer_index),
                    ),
                    base_color: manifold_ui::color::AUDIO_TRIM_BAR_C32,
                    selected: true,
                    hovered: false,
                    muted: false,
                    locked: false,
                    generator: false,
                    alpha: 0.5,
                })
            });
            if !self.clip_rect_scratch.is_empty() || ghost_body.is_some() {
                // Resolve per-clip selection (incl. the marquee case: when the
                // region IS the selection, clips it covers style as selected —
                // same overlap test the bitmap path used, kept WYSIWYG).
                let region = self.ws.ui_root.viewport.selection_region_ref();
                let region_selects_clips =
                    region.is_some() && self.selection.selection_count() == 0;
                let hovered = self.ws.ui_root.viewport.hovered_clip_id();
                // (already cleared above, unconditionally, before this gate)
                // P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D15/D17): grab
                // lift + grid settle + error shake are pure X/Y offsets
                // applied to the SAME `ClipScreenRect` used below for
                // waveforms/thumbnails/names — mutating `cr.rect` in place
                // (rather than only the local `ClipBody`) keeps the whole
                // clip (body, waveform, label) moving together instead of
                // the body sliding out from under its own text.
                let lift_dy = -2.0 * self.overlay.lift_amount();
                let drag_dx = self.overlay.settle_dx_px() + self.overlay.error_shake_offset_px();
                let ghost_alpha = self.overlay.ghost_alpha();
                for cr in &mut self.clip_rect_scratch {
                    let in_marquee = region_selects_clips
                        && region.is_some_and(|r| {
                            manifold_ui::bitmap_renderer::clip_overlaps_region(
                                r,
                                cr.layer_index,
                                cr.start_beat.as_f32(),
                                cr.end_beat.as_f32(),
                            )
                        });
                    let selected = self.selection.is_selected(&cr.clip_id) || in_marquee;
                    let is_hovered = hovered == Some(cr.clip_id.as_str());
                    let is_drag_visual = self.overlay.is_drag_visual_target(&cr.clip_id);
                    if is_drag_visual {
                        cr.rect.x += drag_dx;
                        cr.rect.y += lift_dy;
                    }
                    // D17 "clip split flick": a brief 1px separation between
                    // the two just-split halves, independent of drag state.
                    cr.rect.x += self.ws.ui_root.viewport.split_flick_offset(&cr.clip_id);
                    self.clip_body_scratch
                        .push(manifold_renderer::clip_draw::ClipBody {
                            rect: cr.rect,
                            base_color: cr.base_color,
                            selected,
                            hovered: is_hovered,
                            muted: cr.is_muted,
                            locked: cr.is_locked,
                            generator: cr.is_generator,
                            alpha: if is_drag_visual { ghost_alpha } else { 1.0 },
                        });
                }
                if let Some(ghost) = ghost_body {
                    self.clip_body_scratch.push(ghost);
                }
            }
        }

        // Pass 4b/4b' EMISSION (GPU clip bodies + per-clip waveforms) moved
        // into the seam (`render_main_ui_passes`) below — this block used to
        // continue with `ui.lane_content_scissor`/`emit_clips`/`ui.prepare`/
        // `ui.render` (4b) and `content_gpu.render` (4b') here.

        // Tell the content thread which clips want a thumbnail (non-audio,
        // wide enough), deduped so a stable view sends nothing. The content thread
        // snapshots those clips' live output into the shared atlas.
        {
            const MIN_THUMB_W: f32 = 24.0;
            let thumb_clips: Vec<manifold_core::ClipId> = self
                .clip_rect_scratch
                .iter()
                .filter(|cr| !cr.is_audio && cr.rect.width >= MIN_THUMB_W)
                .map(|cr| cr.clip_id.clone())
                .collect();
            if thumb_clips != self.last_clip_atlas_visible_sent {
                self.send_content_cmd(
                    crate::content_command::ContentCommand::SetClipAtlasVisible(thumb_clips.clone()),
                );
                self.last_clip_atlas_visible_sent = thumb_clips;
            }
        }

        // VQT waterfall input: the six `Application` fields the pass
        // mutates, bundled behind `crate::ui_frame::VqtPassState`, plus the
        // content-thread-published scalars it reads and the caller-resolved
        // scope-cursor position (`Application::scope_hover_uv()` is
        // live-only). Mac-only (module doc deviation #8, `ui_frame.rs`) —
        // constructed unconditionally on macOS whenever GPU state exists;
        // the seam itself gates on `audio_setup_panel.is_open()` etc.
        // Resolved BEFORE the thumbnail block below: `scope_hover_uv()`
        // takes `&self` (whole struct) and cannot run once `thumb_pass`
        // below is holding a `&mut self.clip_thumb_gpu` borrow alive through
        // to the seam call.
        #[cfg(target_os = "macos")]
        let mut vqt_state = {
            let scope_cursor_y = self.scope_hover_uv().map_or(-1.0, |(_, uy, _)| uy);
            Some(crate::ui_frame::VqtPassState {
                spectrogram: &mut self.spectrogram,
                spectrogram_pane: &mut self.spectrogram_pane,
                spectrogram_num_bins: &mut self.spectrogram_num_bins,
                spectrogram_tex_dims: &mut self.spectrogram_tex_dims,
                pending_spectrogram_columns: &mut self.pending_spectrogram_columns,
                pending_spectrogram_scalars: &mut self.pending_spectrogram_scalars,
                content_num_bins: self.content_state.spectrogram_num_bins,
                content_fmin: self.content_state.spectrogram_fmin,
                content_fmax: self.content_state.spectrogram_fmax,
                content_low_hz: self.content_state.spectrogram_low_hz,
                content_mid_hz: self.content_state.spectrogram_mid_hz,
                scope_cursor_y,
                band_dim: self.ws.ui_root.open_fire_mode_drawer_band(),
            })
        };
        #[cfg(not(target_os = "macos"))]
        let mut vqt_state: Option<crate::ui_frame::VqtPassState> = None;

        // Pass 4b″ input: Clip thumbnails (§24 5c) — resolve each visible
        // generator/video clip's atlas cell (published by the content
        // thread) into a `ThumbQuad`, centre-cropped to the body aspect.
        // The actual blit (`ClipThumbGpu::render`) moved into the seam
        // below as `MainUiPassInputs::thumb` — this block only builds the
        // input; `thumb_pass` stays `None` (skips the pass, §3) whenever the
        // atlas/bridge isn't resolved, quads end up empty, or off-macOS.
        #[cfg(target_os = "macos")]
        let mut thumb_pass: Option<crate::ui_frame::ThumbPass> = None;
        #[cfg(not(target_os = "macos"))]
        let thumb_pass: Option<crate::ui_frame::ThumbPass> = None;
        #[cfg(target_os = "macos")]
        if !self.clip_rect_scratch.is_empty()
            && !self.content_state.clip_atlas_layout.is_empty()
        {
            // Single shared surface (BUG-119) — no front-buffer index to resolve;
            // the imported texture always reflects the content thread's latest
            // cell blits directly (no clear after init, so at worst a cell mid-blit
            // this frame shows valid-old or valid-new pixels, never blank).
            if let Some(atlas) = self.ui_clip_atlas_texture.as_ref() {
                // clip → (filmstrip cell index → atlas cell), from the published
                // layout. Each clip tiles its captured bar cells across its body.
                let mut strips_of: ahash::AHashMap<&str, ahash::AHashMap<u32, u32>> =
                    ahash::AHashMap::new();
                for (cid, idx, cell) in &self.content_state.clip_atlas_layout {
                    strips_of.entry(cid.as_str()).or_default().insert(*idx, *cell);
                }
                let cell_aspect = crate::content_pipeline::CLIP_ATLAS_CELL_W as f32
                    / crate::content_pipeline::CLIP_ATLAS_CELL_H as f32;
                let inv_cols = 1.0 / crate::content_pipeline::CLIP_ATLAS_COLS as f32;
                let inv_rows = 1.0 / crate::content_pipeline::CLIP_ATLAS_ROWS as f32;
                let bpb = self.ws.ui_root.viewport.beats_per_bar() as f64;
                self.clip_thumb_quad_scratch.clear();
                // §F aspect-locked window scratch — reused across clips this frame
                // (cleared per clip; grows once), like `strips_of` above.
                let mut thumb_cells: Vec<(u32, f32)> = Vec::new();
                let mut thumb_windows: Vec<(u32, f32, f32)> = Vec::new();
                for cr in &self.clip_rect_scratch {
                    // Match the SetClipAtlasVisible filter so a clip too narrow to
                    // have requested a cell never draws one.
                    if cr.is_audio || cr.rect.width < 24.0 {
                        continue;
                    }
                    let Some(strip) = strips_of.get(cr.clip_id.as_str()) else {
                        continue;
                    };
                    // Reserve the bottom name-strip band: the thumbnail tiles only
                    // the PREVIEW area above it (mockup `.clip .body{bottom:16px}`),
                    // so the layer-coloured strip + name below are never covered.
                    // Same `clip_strip_height` the clip-body pass uses → they agree.
                    // Then inset by CLIP_THUMB_INSET on top/left/right (and leave the
                    // same gap above the strip) so the darker well frames the
                    // thumbnail as a dedicated panel instead of bleeding to the edge.
                    let strip_h = manifold_renderer::clip_draw::clip_strip_height(cr.rect.height)
                        .unwrap_or(0.0);
                    let m = manifold_ui::color::CLIP_THUMB_INSET;
                    let preview_h = (cr.rect.height - strip_h).max(1.0);
                    let body = manifold_ui::node::Rect::new(
                        cr.rect.x + m,
                        cr.rect.y + m,
                        (cr.rect.width - 2.0 * m).max(1.0),
                        (preview_h - 2.0 * m).max(1.0),
                    );
                    let body_right = body.x + body.width;
                    let start_b = cr.start_beat.as_f32() as f64;
                    let dur_b = (cr.end_beat - cr.start_beat).as_f32() as f64;
                    let count = crate::clip_filmstrip::cell_count(
                        crate::clip_filmstrip::clip_bar_count(dur_b, bpb),
                    );
                    // §F/§G: collect the captured cells with their on-screen start x,
                    // then lay a continuous grid of aspect-locked windows over the body,
                    // each filled by the nearest captured frame — gapless and regularly
                    // spaced even when only some bars have been swept/captured.
                    thumb_cells.clear();
                    for (&idx, &cell) in strip {
                        if idx >= count {
                            continue; // stale layout entry (clip shortened since capture)
                        }
                        let (sb, _eb) =
                            crate::clip_filmstrip::cell_beat_range(idx, start_b, dur_b, bpb);
                        thumb_cells.push((cell, self.ws.ui_root.viewport.beat_f64_to_pixel(sb)));
                    }
                    thumb_cells.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
                    // Window width = a project-aspect frame at the lane height, decoupled
                    // from bar width — the §F fix for the squished low-zoom filmstrip.
                    let win_w = body.height * cell_aspect;
                    crate::clip_filmstrip::grid_windows(
                        &thumb_cells,
                        body.x,
                        body_right,
                        win_w,
                        &mut thumb_windows,
                    );
                    for &(cell, x0, w) in &thumb_windows {
                        let sub = manifold_ui::node::Rect::new(x0, body.y, w, body.height);
                        // Atlas cell UV in the non-square COLS×ROWS grid.
                        let gx = (cell % crate::content_pipeline::CLIP_ATLAS_COLS) as f32;
                        let gy = (cell / crate::content_pipeline::CLIP_ATLAS_COLS) as f32;
                        let (u0, v0) = (gx * inv_cols, gy * inv_rows);
                        let (u1, v1) = (u0 + inv_cols, v0 + inv_rows);
                        // A full aspect-locked window shows the whole frame (no crop);
                        // only a clamped partial last window is centre-cropped.
                        let sub_aspect = (w / body.height.max(1.0)).max(0.01);
                        let (uu0, vv0, uu1, vv1) = if sub_aspect >= cell_aspect {
                            let f = cell_aspect / sub_aspect; // crop height
                            let vc = (v0 + v1) * 0.5;
                            let h = (v1 - v0) * f * 0.5;
                            (u0, vc - h, u1, vc + h)
                        } else {
                            let f = sub_aspect / cell_aspect; // crop width
                            let uc = (u0 + u1) * 0.5;
                            let cw = (u1 - u0) * f * 0.5;
                            (uc - cw, v0, uc + cw, v1)
                        };
                        self.clip_thumb_quad_scratch.push(
                            manifold_renderer::clip_thumb_gpu::ThumbQuad {
                                rect: sub,
                                body_rect: body,
                                radius: manifold_ui::color::CLIP_RADIUS,
                                uv_min: [uu0, vv0],
                                uv_max: [uu1, vv1],
                            },
                        );
                    }
                }
                if !self.clip_thumb_quad_scratch.is_empty()
                    && let Some(thumb) = self.clip_thumb_gpu.as_mut()
                {
                    thumb_pass = Some(crate::ui_frame::ThumbPass {
                        gpu: thumb,
                        atlas,
                        quads: &self.clip_thumb_quad_scratch,
                    });
                }
            }
        }

        // Pass 4c (lane / stem / overview / collapsed-group panel bitmaps)
        // moved entirely into the seam below — it reads
        // `ui_root.viewport.overview_rect()`/`collapsed_group_rects()` and
        // `inputs.layer_bitmap_gpu` directly, no caller-side resolution
        // needed (module doc, `ui_frame.rs`).

        // Timeline overlays (region highlight / insert cursor / beat markers) as
        // GPU rects (§24 5b — no longer baked into a per-layer bitmap). Resolved
        // here while `self` is free; drawn inside the seam below (region/cursor/
        // markers under the clip names). The insert cursor's layer comes from
        // the app's selection (it owns the resolved layer id).
        let insert_layer = self
            .selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        let timeline_overlays = self.ws.ui_root.viewport.timeline_overlays(
            insert_layer,
            self.selection.has_insert_cursor(),
            &mut self.timeline_marker_scratch,
        );

        // Pass 5 input: automation lanes + landing flash, resolved
        // caller-side like `timeline_overlays` above (module doc,
        // `ui_frame.rs`) — the actual draw calls moved into the seam.
        let automation_lanes = self
            .ws
            .ui_root
            .viewport
            .automation_lane_screens(&self.content_state.automation_latched_params);
        let landing_flash = self.overlay.landing_flash();

        // ── The seam call: Passes 4a→5 + VQT + overlay dirty-clear, all in
        // one shared function also called by the headless harness. Gated on
        // `(ui_renderer, blit_pipeline, blit_sampler)` all `Some` — see
        // module doc deviation #9 (`ui_frame.rs`) for why this reproduces
        // the old per-pass gating on every reachable frame.
        if let (Some(ui), Some(blit_pipeline), Some(blit_sampler)) =
            (self.ui_renderer.as_mut(), &self.blit_pipeline, &self.blit_sampler)
        {
            crate::ui_frame::render_main_ui_passes(
                &gpu.device,
                ui,
                &mut self.ws.ui_root,
                offscreen,
                logical_w,
                logical_h,
                scale,
                crate::ui_frame::MainUiPassInputs {
                    layer_bitmap_gpu: self.layer_bitmap_gpu.as_mut(),
                    clip_bodies: &self.clip_body_scratch,
                    clip_rects: &self.clip_rect_scratch,
                    clip_content_gpu: self.clip_content_gpu.as_mut(),
                    thumb: thumb_pass,
                    timeline_overlays,
                    markers: &self.timeline_marker_scratch,
                    landing_flash,
                    automation_lanes: &automation_lanes,
                    cursor_pos: self.cursor_pos,
                    text_input: &self.text_input,
                    frame_timer: &self.frame_timer,
                    vqt: vqt_state.as_mut(),
                    blit_pipeline,
                    blit_sampler,
                    // The seam owns + commits the offscreen "Frame" encoder, so
                    // the async GPU-time handler moves inside it (fed this sink).
                    gpu_sink: self.ui_profile.gpu_sink(),
                },
            );
        }
        self.ui_profile.add("present.main_ui_passes", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Late drawable acquisition ──
        // Acquire the drawable as late as possible to minimize time blocking on
        // WindowServer IPC. All GPU work is already committed to the offscreen
        // texture above — this is just a single fullscreen blit.
        //
        // Skip entirely on resize frames: set_drawable_size reconfigures the
        // drawable pool, and nextDrawable can block up to 1s during the
        // reconfiguration. The offscreen render is still committed above —
        // it just won't be blitted to screen this frame.
        if self.ws.surface_resized_this_frame {
            self.ws.surface_resized_this_frame = false;
            return;
        }
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
                None => {
                    log::warn!("No drawable available — skipping frame");
                    return;
                }
            }
        };
        self.ui_profile.add("present.next_drawable", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Blit offscreen → drawable + present ──
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        let blit_pipeline = match &self.blit_pipeline {
            Some(p) => p,
            None => return,
        };
        let blit_sampler = match &self.blit_sampler {
            Some(s) => s,
            None => return,
        };

        let mut present_enc = gpu.device.create_encoder("Present");
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
            true, // store: must write to drawable for present
            "Offscreen → Drawable",
        );
        present_enc.present_drawable(&drawable);
        present_enc.commit();
        self.ui_profile.add("present.blit_present", pseg.elapsed());

        // BUG-060 surface dump: attribute live stale-pixel dirt to a surface.
        // Runs only on dirty-present frames, so scrolling produces fresh dumps
        // and idle frames cost nothing.
        if let Some(every) = bug060_dump_every() {
            let n = BUG060_DUMP_FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n.is_multiple_of(every) {
                let footer = self.ws.ui_root.layout.footer();
                let inspector = self.ws.ui_root.layout.inspector();
                eprintln!(
                    "[BUG-060] dump #{n}: sf={sf} offscreen={}x{} footer=({:.1},{:.1} {:.1}x{:.1}) inspector=({:.1},{:.1} {:.1}x{:.1})",
                    offscreen.width,
                    offscreen.height,
                    footer.x,
                    footer.y,
                    footer.width,
                    footer.height,
                    inspector.x,
                    inspector.y,
                    inspector.width,
                    inspector.height,
                );
                bug060_dump_png(&gpu.device, offscreen, "/tmp/bug060_offscreen.png");
                if let Some(atlas) = self.ui_cache_manager.as_ref().and_then(|cm| cm.atlas_texture())
                {
                    bug060_dump_png(&gpu.device, atlas, "/tmp/bug060_atlas.png");
                }
            }
        }
    }

    /// Re-blit the cached offscreen onto a fresh drawable and present it,
    /// without touching `offscreen_dirty`. Shared by two callers in
    /// `present_all_windows`: the steady-state fast path (nothing changed —
    /// clears `offscreen_dirty` itself, which is already false) and
    /// admission control (something *did* change, but the GPU is too far
    /// behind to encode a new frame this tick — leaves `offscreen_dirty`
    /// set so the pending redraw runs once the backlog clears). Both
    /// present the identical cached pixels; only whether the redraw is
    /// considered "done" differs, so the callers own that bookkeeping, not
    /// this helper.
    pub(crate) fn represent_cached_offscreen(&mut self, window_id: winit::window::WindowId, pseg: &mut std::time::Instant) {
        let Some(gpu) = &self.gpu else { return };
        let Some(offscreen) = self.ws.ui_offscreen.as_ref() else {
            return;
        };
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
        self.ui_profile
            .add("present.fast_next_drawable", pseg.elapsed());
        *pseg = std::time::Instant::now();
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        if let (Some(blit_p), Some(blit_s)) = (&self.blit_pipeline, &self.blit_sampler) {
            let mut enc = gpu.device.create_encoder("Re-present");
            enc.draw_fullscreen(
                blit_p,
                &drawable_tex,
                &[
                    manifold_gpu::GpuBinding::Texture {
                        binding: 0,
                        texture: offscreen,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 1,
                        sampler: blit_s,
                    },
                ],
                false,
                true,
                "Offscreen → Drawable",
            );
            enc.present_drawable(&drawable);
            enc.commit();
        }
        self.ui_profile.add("present.fast_blit_present", pseg.elapsed());
    }
}
