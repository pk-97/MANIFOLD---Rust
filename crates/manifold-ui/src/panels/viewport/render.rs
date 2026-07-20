//! Viewport rendering: tree build, ruler / markers / track backgrounds, the
//! overview minimap and collapsed-group bitmaps, and the scroll update-in-place
//! paths. See `docs/TIMELINE_API_DESIGN.md` §3.6.

use super::*;

impl TimelineViewportPanel {
    /// Update insert cursor ruler marker position without rebuilding.
    /// Track-area cursor is painted into bitmap.
    pub(super) fn sync_insert_cursor_ruler(&self, tree: &mut UITree) {
        if let Some(cursor_id) = self.insert_cursor_ruler_id {
            let px = self.beat_to_pixel(self.insert_cursor_beat);
            let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();
            tree.set_visible(cursor_id, in_view);
            if in_view {
                let marker_s = color::INSERT_CURSOR_RULER_MARKER_SIZE;
                tree.set_bounds(
                    cursor_id,
                    Rect::new(
                        px - marker_s * 0.5,
                        self.ruler_rect.y + self.ruler_rect.height - marker_s,
                        marker_s,
                        marker_s,
                    ),
                );
            }
        }
    }

    /// Build clip miniatures in the overview strip.
    /// From Unity OverviewStripPanel.BuildPanel (lines 218-270).
    /// Renders small colored rects for each clip, a viewport indicator,
    /// and the playhead position.
    /// Repaint the overview strip bitmap. Call once per frame before GPU upload.
    /// Paints ALL clips (no cap) into a small CPU pixel buffer, then overlays
    /// the viewport indicator and playhead. Group layers are excluded.
    pub fn repaint_overview(&mut self) {
        let scale = self.render_scale;
        let tex_w = (self.overview_rect.width * scale).round().max(1.0) as usize;
        let tex_h = (self.overview_rect.height * scale).round().max(1.0) as usize;

        // Dirty-checking: skip if nothing changed at all.
        let ppb = self.mapper.pixels_per_beat();
        let scroll_x = self.scroll_x_beats.as_f32();
        let playhead = self.playhead_beat.as_f32();
        if !self.overview_dirty
            && !self.overview_clips_dirty
            && self.overview_last_playhead == playhead
            && self.overview_last_scroll_x == scroll_x
            && self.overview_last_ppb == ppb
            && self.overview_last_track_count == self.tracks.len()
            && self.overview_last_width == self.overview_rect.width
        {
            return;
        }

        // Check if clip layer needs repaint (expensive) vs overlay-only (cheap).
        let size_changed = tex_w != self.overview_tex_w || tex_h != self.overview_tex_h;
        let clips_need_repaint = self.overview_clips_dirty
            || size_changed
            || self.overview_last_track_count != self.tracks.len();

        self.overview_last_playhead = playhead;
        self.overview_last_scroll_x = scroll_x;
        self.overview_last_ppb = ppb;
        self.overview_last_track_count = self.tracks.len();
        self.overview_last_width = self.overview_rect.width;
        self.overview_tex_w = tex_w;
        self.overview_tex_h = tex_h;

        let total = tex_w * tex_h;

        if self.total_clip_count() == 0 || self.tracks.is_empty() {
            self.overview_pixels.resize(total, Color32::TRANSPARENT);
            self.overview_pixels.fill(Color32::TRANSPARENT);
            self.overview_dirty = true;
            return;
        }

        // Content duration for normalization
        let mut max_beat = 0.0f32;
        for clip in self.clips_by_layer.iter().flatten() {
            let end = clip.start_beat.as_f32() + clip.duration_beats.as_f32();
            if end > max_beat {
                max_beat = end;
            }
        }
        if max_beat <= 0.0 {
            self.overview_pixels.resize(total, Color32::TRANSPARENT);
            self.overview_pixels.fill(Color32::TRANSPARENT);
            self.overview_dirty = true;
            return;
        }

        // ── Layer 1: Clip layer (cached, only repainted on clip data change) ──
        if clips_need_repaint {
            self.overview_clip_pixels
                .resize(total, Color32::TRANSPARENT);
            self.overview_clip_pixels.fill(Color32::TRANSPARENT);

            // Remap: skip group layers
            let mut non_group_row: Vec<Option<usize>> = Vec::with_capacity(self.tracks.len());
            let mut non_group_count: usize = 0;
            for track in &self.tracks {
                if track.is_group {
                    non_group_row.push(None);
                } else {
                    non_group_row.push(Some(non_group_count));
                    non_group_count += 1;
                }
            }

            if non_group_count > 0 {
                let row_h = tex_h as f32 / non_group_count as f32;

                for clip in self.clips_by_layer.iter().flatten() {
                    let row = match non_group_row.get(clip.layer_index).copied().flatten() {
                        Some(r) => r,
                        None => continue,
                    };
                    let start_norm = clip.start_beat.as_f32() / max_beat;
                    let end_norm =
                        (clip.start_beat.as_f32() + clip.duration_beats.as_f32()) / max_beat;
                    let x = (start_norm * tex_w as f32).round() as i32;
                    let w = ((end_norm - start_norm) * tex_w as f32).round().max(1.0) as i32;
                    let y = (row as f32 * row_h).round() as i32;
                    let h = row_h.round().max(1.0) as i32;

                    bitmap_painter::fill_rect(
                        &mut self.overview_clip_pixels,
                        tex_w,
                        tex_h,
                        x,
                        y,
                        w,
                        h,
                        clip.color,
                    );
                }
            }
            self.overview_clips_dirty = false;
        }

        // ── Layer 2: Composite — copy cached clips, then overlay indicator + playhead ──
        self.overview_pixels.resize(total, Color32::TRANSPARENT);
        self.overview_pixels
            .copy_from_slice(&self.overview_clip_pixels);

        // Viewport indicator (semi-transparent blue)
        if ppb > 0.0 {
            let viewport_width_beats = self.tracks_rect.width / ppb;
            let vp_start_norm = scroll_x / max_beat;
            let vp_width_norm = viewport_width_beats / max_beat;
            let vp_x = (vp_start_norm * tex_w as f32).round() as i32;
            let vp_w = (vp_width_norm * tex_w as f32).round().min(tex_w as f32) as i32;
            bitmap_painter::fill_rect(
                &mut self.overview_pixels,
                tex_w,
                tex_h,
                vp_x,
                0,
                vp_w,
                tex_h as i32,
                color::OVERVIEW_VIEWPORT,
            );
            bitmap_painter::draw_border(
                &mut self.overview_pixels,
                tex_w,
                tex_h,
                vp_x,
                0,
                vp_w,
                tex_h as i32,
                color::OVERVIEW_VIEWPORT_BORDER,
                1,
            );
        }

        // Playhead (red line, 1-2px)
        let ph_norm = playhead / max_beat;
        let ph_x = (ph_norm * tex_w as f32).round().clamp(0.0, tex_w as f32) as i32;
        let ph_w = (1.0 * scale).round().max(1.0) as i32;
        bitmap_painter::fill_rect(
            &mut self.overview_pixels,
            tex_w,
            tex_h,
            ph_x,
            0,
            ph_w,
            tex_h as i32,
            color::OVERVIEW_PLAYHEAD,
        );

        self.overview_dirty = true;
    }

    /// Overview bitmap data for GPU upload. Returns (pixels, w, h) if dirty.
    pub fn overview_bitmap(&mut self) -> Option<(&[Color32], usize, usize)> {
        if self.overview_dirty && self.overview_tex_w > 0 && self.overview_tex_h > 0 {
            self.overview_dirty = false;
            Some((
                &self.overview_pixels,
                self.overview_tex_w,
                self.overview_tex_h,
            ))
        } else {
            None
        }
    }

    /// Overview rect (screen-space) for GPU rendering.
    pub fn overview_rect(&self) -> Rect {
        self.overview_rect
    }

    /// Repaint collapsed group bitmaps. Call once per frame before GPU upload.
    pub fn repaint_collapsed_groups(&mut self) {
        let (min_beat, max_beat) = self.visible_beat_range();
        let viewport_w = self.tracks_rect.width;
        let scale = self.render_scale;

        for (i, bmp_opt) in self.collapsed_group_bitmaps.iter_mut().enumerate() {
            let bmp = match bmp_opt.as_mut() {
                Some(b) => b,
                None => continue,
            };
            let track = &self.tracks[i];
            if !track.is_group || !track.is_collapsed || track.child_layer_indices.is_empty() {
                continue;
            }

            let track_h = self.mapper.get_layer_height(i);
            if track_h <= 0.0 || viewport_w <= 0.0 {
                continue;
            }

            // Count child clips for dirty check
            let mut child_clip_count = 0usize;
            for &ci in &track.child_layer_indices {
                if ci < self.clips_by_layer.len() {
                    child_clip_count += self.clips_by_layer[ci].len();
                }
            }

            // Dirty-checking
            if !bmp.dirty
                && bmp.last_min_beat == min_beat
                && bmp.last_max_beat == max_beat
                && bmp.last_viewport_w == viewport_w
                && bmp.last_track_h == track_h
                && bmp.last_clip_count == child_clip_count
            {
                continue;
            }
            bmp.last_min_beat = min_beat;
            bmp.last_max_beat = max_beat;
            bmp.last_viewport_w = viewport_w;
            bmp.last_track_h = track_h;
            bmp.last_clip_count = child_clip_count;

            let tex_w = (viewport_w * scale).round().max(1.0) as usize;
            let tex_h = (track_h * scale).round().max(1.0) as usize;
            let total = tex_w * tex_h;
            bmp.pixels.resize(total, Color32::TRANSPARENT);
            bmp.pixels.fill(Color32::TRANSPARENT);
            bmp.tex_w = tex_w;
            bmp.tex_h = tex_h;

            let child_count = track.child_layer_indices.len();
            let rows_per_child = tex_h as f32 / child_count.max(1) as f32;
            let beat_range = max_beat - min_beat;
            if beat_range <= 0.0 {
                bmp.dirty = true;
                continue;
            }

            for (ci, &child_idx) in track.child_layer_indices.iter().enumerate() {
                let child_y = (ci as f32 * rows_per_child).round() as i32;
                let child_h = rows_per_child.round().max(1.0) as i32;

                let child_clips = if child_idx < self.clips_by_layer.len() {
                    &self.clips_by_layer[child_idx]
                } else {
                    continue;
                };

                for clip in child_clips {
                    let clip_start = clip.start_beat.as_f32();
                    let clip_end = clip_start + clip.duration_beats.as_f32();
                    if clip_end < min_beat || clip_start > max_beat {
                        continue;
                    }

                    let x_norm = (clip_start - min_beat) / beat_range;
                    let x2_norm = (clip_end - min_beat) / beat_range;
                    let x = (x_norm * tex_w as f32).round().max(0.0) as i32;
                    let x2 = (x2_norm * tex_w as f32).round().min(tex_w as f32) as i32;
                    let w = (x2 - x).max(1);

                    bitmap_painter::fill_rect(
                        &mut bmp.pixels,
                        tex_w,
                        tex_h,
                        x,
                        child_y,
                        w,
                        child_h,
                        clip.color,
                    );
                }
            }
            bmp.dirty = true;
        }
    }

    /// Iterate collapsed group bitmaps that need GPU upload.
    /// Yields (track_index, pixels, tex_w, tex_h) for dirty groups.
    pub fn dirty_collapsed_group_iter(
        &mut self,
    ) -> impl Iterator<Item = (usize, &[Color32], usize, usize)> {
        self.collapsed_group_bitmaps
            .iter_mut()
            .enumerate()
            .filter_map(|(i, opt)| {
                opt.as_mut().and_then(|bmp| {
                    if bmp.dirty && bmp.tex_w > 0 && bmp.tex_h > 0 {
                        bmp.dirty = false;
                        Some((i, bmp.pixels.as_slice(), bmp.tex_w, bmp.tex_h))
                    } else {
                        None
                    }
                })
            })
    }

    /// Screen-space rects for collapsed group bitmaps (for GPU rendering).
    /// Returns (layer_index_offset, rect) where layer_index_offset = 2000 + track_index.
    pub fn collapsed_group_rects(&self) -> Vec<(usize, Rect)> {
        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;

        let mut rects = Vec::new();
        for (i, bmp_opt) in self.collapsed_group_bitmaps.iter().enumerate() {
            if bmp_opt.is_none() {
                continue;
            }
            let h = self.track_height(i);
            if h <= 0.0 {
                continue;
            }
            let y = self.track_y(i);
            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            if clamped_h <= 0.0 {
                continue;
            }
            rects.push((2000 + i, Rect::new(tr.x, clamped_y, tr.width, clamped_h)));
        }
        rects
    }

    /// Resting background colour for track lane `i`: the zebra stripe, lifted
    /// one ramp step when it is the focused lane (§19 timeline echo — the same
    /// lift the inspector card's well gets). Mute does not tint the lane, to
    /// match Ableton. The single source for both `build_track_backgrounds` and
    /// the in-place `sync_active_track_lane` recolor.
    pub(super) fn track_bg_color(&self, i: usize) -> Color32 {
        // The selected lane gets its OWN colour (a muted navy), not a brightened
        // zebra stripe — a lift was too close to the alternating greys to read as
        // "selected".
        if self.active_track_index == Some(i) {
            return color::TRACK_BG_SELECTED;
        }
        // Parity is counted over visible rows (see `track_zebra_even`), not the raw
        // index, so collapsed group children don't flip the stripe for lanes below.
        let even = self
            .track_zebra_even
            .get(i)
            .copied()
            .unwrap_or(i.is_multiple_of(2));
        if even {
            color::TRACK_BG
        } else {
            color::TRACK_BG_ALT
        }
    }

    pub(super) fn build_track_backgrounds(&mut self, tree: &mut UITree) {
        self.track_bg_ids.clear();
        self.track_bg_groups.clear();

        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;

        // Pre-allocate ALL tracks (including off-screen) for update-in-place.
        // Off-screen tracks get set_visible(false).
        for i in 0..self.tracks.len() {
            let track = &self.tracks[i];
            let y = self.track_y(i);
            let h = self.mapper.get_layer_height(i);

            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            let visible = clamped_h > 0.0 && y + h >= tr_top && y <= tr_bottom;

            // Zebra stripe, lifted one ramp step when focused
            // (§19 echo) — all owned by `track_bg_color` so build and the in-place
            // recolor (`sync_active_track_lane`) can never drift.
            let style = UIStyle {
                bg_color: self.track_bg_color(i),
                ..UIStyle::default()
            };

            let bg_id = tree.add_panel(
                None,
                tr.x,
                if visible { clamped_y } else { tr_top },
                tr.width,
                if visible { clamped_h } else { 0.0 },
                style,
            );
            if !visible {
                tree.set_visible(bg_id, false);
            }
            self.track_bg_ids.push(bg_id);

            // Bottom separator — always allocated
            let (sep_h, sep_color) = if track.is_group {
                (color::GROUP_SEPARATOR_HEIGHT, color::GROUP_SEPARATOR_COLOR)
            } else {
                (color::TRACK_SEPARATOR_HEIGHT, color::SEPARATOR_COLOR)
            };
            let sep_y = y + h - sep_h;
            let sep_vis = visible && sep_y + sep_h > tr_top && sep_y < tr_bottom;
            let separator_id = tree.add_panel(
                None,
                tr.x,
                if sep_vis { sep_y.max(tr_top) } else { tr_top },
                tr.width,
                if sep_vis {
                    (sep_y + sep_h).min(tr_bottom) - sep_y.max(tr_top)
                } else {
                    0.0
                },
                UIStyle {
                    bg_color: sep_color,
                    ..UIStyle::default()
                },
            );
            if !sep_vis {
                tree.set_visible(separator_id, false);
            }

            self.track_bg_groups.push(TrackBgGroup { bg_id, separator_id });
        }

        // Top separator is painted into the first layer's bitmap (not a UITree node)
        // because the layer bitmap textures render on top of UITree panels in a later
        // GPU pass, covering any UITree-based separator.
    }

    // build_grid_lines: REMOVED — grid lines are now painted into per-layer bitmaps
    // by LayerBitmapRenderer.paint_grid_lines() (matching Unity exactly).

    pub(super) fn build_ruler(&mut self, tree: &mut UITree) {
        self.ruler_tick_ids.clear();
        self.ruler_label_ids.clear();

        let (min_beat, max_beat) = self.visible_beat_range();
        let bpb = self.beats_per_bar as f32;
        let ppb = self.mapper.pixels_per_beat();
        let subdiv = self.grid_subdivision();

        // ── Tick step (controls which tick marks appear) ──
        let tick_step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        // ── Label step (adaptive — ensures labels never overlap) ──
        // Find the smallest musically-meaningful interval where labels
        // are at least MIN_LABEL_SPACING pixels apart.
        const MIN_LABEL_SPACING: f32 = 50.0;
        let label_step: f32 = if ppb >= MIN_LABEL_SPACING {
            // Enough room for per-beat labels (bar.beat format)
            1.0
        } else if bpb * ppb >= MIN_LABEL_SPACING {
            // Enough room for per-bar labels
            bpb
        } else {
            // Skip bars — double until labels fit
            let bar_px = bpb * ppb;
            let mut n_bars = 2.0_f32;
            while n_bars * bar_px < MIN_LABEL_SPACING && n_bars <= 1024.0 {
                n_bars *= 2.0;
            }
            bpb * n_bars
        };

        let bar_skip = self.bar_skip();
        let start = (min_beat / tick_step).floor() * tick_step;
        let mut beat = start;
        let mut count = 0;
        let ruler_bottom = self.ruler_rect.y + self.ruler_rect.height;

        while beat <= max_beat && count < MAX_RULER_TICKS {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;
                let is_beat = (beat % 1.0).abs() < 0.001;
                let is_label_beat = (beat % label_step).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

                // Labeled bars get taller ticks for visual anchoring
                let tick_h = if is_label_beat && is_bar {
                    RULER_BAR_TICK_H + 4.0
                } else if is_bar {
                    RULER_BAR_TICK_H
                } else if is_beat {
                    RULER_BEAT_TICK_H
                } else {
                    4.0
                };

                let tick_color = if is_label_beat && is_bar {
                    color::TEXT_NORMAL
                } else if is_bar {
                    color::TEXT_SUBTLE
                } else {
                    color::TEXT_FAINT
                };

                // Tick mark (bottom-aligned)
                let id = tree.add_panel(
                    self.viewport_clip_id,
                    px,
                    ruler_bottom - tick_h,
                    RULER_TICK_W,
                    tick_h,
                    UIStyle {
                        bg_color: tick_color,
                        ..UIStyle::default()
                    },
                );
                self.ruler_tick_ids.push(id);

                // Label (only at label_step intervals to prevent overlap)
                // Skip labels at beats where a marker exists — markers take priority.
                let has_marker_at_beat = self
                    .markers
                    .iter()
                    .any(|m| (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty());
                if is_label_beat && !has_marker_at_beat {
                    let bar_num = (beat / bpb).floor() as i32 + 1;
                    let beat_in_bar = ((beat % bpb) + 0.001).floor() as i32 + 1;
                    let label = if is_bar {
                        format!("{}", bar_num)
                    } else {
                        format!("{}.{}", bar_num, beat_in_bar)
                    };

                    let label_y = self.ruler_rect.y + 2.0;
                    let id = tree.add_label(
                        self.viewport_clip_id,
                        px + 2.0,
                        label_y,
                        RULER_LABEL_W,
                        RULER_LABEL_H,
                        &label,
                        UIStyle {
                            text_color: if is_bar {
                                color::TEXT_NORMAL
                            } else {
                                color::TEXT_DIMMED
                            },
                            font_size: RULER_FONT_SIZE,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );
                    self.ruler_label_ids.push(id);
                }

                count += 1;
            }
            beat += tick_step;
        }
    }

    // build_clips: REMOVED — clips are now painted into per-layer bitmaps
    // by LayerBitmapRenderer (matching Unity's LayerBitmapPainter.DrawClip exactly).

    // build_selection_region: REMOVED — selection region is now painted into
    // per-layer bitmaps by LayerBitmapRenderer (matching Unity exactly).

    pub(super) fn build_export_markers(&mut self, tree: &mut UITree) {
        // Always pre-allocate all 3 export marker nodes for update-in-place.
        // Use set_visible(false) when not needed.
        let marker_w = 2.0;
        let marker_h = self.ruler_rect.height + self.tracks_rect.height;
        let marker_style = UIStyle {
            bg_color: color::EXPORT_MARKER_COLOR,
            ..UIStyle::default()
        };

        // In marker
        let in_px = self.beat_to_pixel(self.export_in_beat);
        let export_in_marker_id = tree.add_panel(
            self.viewport_clip_id,
            in_px - marker_w * 0.5,
            self.ruler_rect.y,
            marker_w,
            marker_h,
            marker_style,
        );
        self.export_in_marker_id = Some(export_in_marker_id);

        // Range highlight
        let out_px = self.beat_to_pixel(self.export_out_beat);
        let range_left = in_px.max(self.tracks_rect.x);
        let range_right = out_px.min(self.tracks_rect.x_max());
        let range_w = (range_right - range_left).max(0.0);
        let export_range_id = tree.add_panel(
            self.viewport_clip_id,
            range_left,
            self.tracks_rect.y,
            range_w,
            self.tracks_rect.height,
            UIStyle {
                bg_color: color::EXPORT_RANGE_HIGHLIGHT,
                ..UIStyle::default()
            },
        );
        self.export_range_id = Some(export_range_id);

        // Out marker
        let export_out_marker_id = tree.add_panel(
            self.viewport_clip_id,
            out_px - marker_w * 0.5,
            self.ruler_rect.y,
            marker_w,
            marker_h,
            marker_style,
        );
        self.export_out_marker_id = Some(export_out_marker_id);

        // Apply visibility
        let enabled = self.export_range_enabled;
        let has_out = self.export_out_beat > self.export_in_beat;
        let in_visible =
            enabled && in_px >= self.tracks_rect.x && in_px <= self.tracks_rect.x_max();
        let out_visible = enabled
            && has_out
            && out_px >= self.tracks_rect.x
            && out_px <= self.tracks_rect.x_max();
        let range_visible = enabled && has_out && range_w > 0.0;

        if !in_visible {
            tree.set_visible(export_in_marker_id, false);
        }
        if !range_visible {
            tree.set_visible(export_range_id, false);
        }
        if !out_visible {
            tree.set_visible(export_out_marker_id, false);
        }
    }

    /// Build insert cursor ruler marker only. Track-area cursor is painted
    /// into the per-layer bitmap by LayerBitmapRenderer.
    pub(super) fn build_insert_cursor_ruler(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.insert_cursor_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        let marker_s = color::INSERT_CURSOR_RULER_MARKER_SIZE;
        let insert_cursor_ruler_id = tree.add_panel(
            self.viewport_clip_id,
            px - marker_s * 0.5,
            self.ruler_rect.y + self.ruler_rect.height - marker_s,
            marker_s,
            marker_s,
            UIStyle {
                bg_color: color::INSERT_CURSOR_BLUE,
                ..UIStyle::default()
            },
        );
        self.insert_cursor_ruler_id = Some(insert_cursor_ruler_id);
        if !in_view {
            tree.set_visible(insert_cursor_ruler_id, false);
        }
    }

    /// Build timeline marker vertical lines and flags in the ruler.
    pub(super) fn build_markers(&mut self, tree: &mut UITree) {
        self.marker_node_ids.clear();
        self.marker_groups.clear();

        let flag_w = color::MARKER_FLAG_WIDTH;
        let flag_h = color::MARKER_FLAG_HEIGHT;

        // Pre-allocate ALL markers (including off-screen) for update-in-place.
        // Off-screen markers get set_visible(false).
        for marker in &self.markers {
            // One geometry source for flag node + hit-test (marker_flag_rect).
            let flag = self.marker_flag_rect(marker.beat);
            let flag_x = flag.x;
            let flag_y = flag.y;
            let px = self.beat_to_pixel(marker.beat);
            let in_view =
                px >= self.tracks_rect.x - flag_w && px <= self.tracks_rect.x_max() + flag_w;

            let mc = color::marker_color_to_color32(marker.color);
            let is_selected = self.selected_marker_ids.contains(&marker.id);

            let flag_color = if is_selected {
                color::lighten(mc, 40)
            } else {
                mc
            };
            let flag_id = tree.add_panel(
                self.viewport_clip_id,
                flag_x,
                flag_y,
                flag_w,
                flag_h,
                UIStyle {
                    bg_color: flag_color,
                    ..UIStyle::default()
                },
            );
            if !in_view {
                tree.set_visible(flag_id, false);
            }
            self.marker_node_ids.push(flag_id);

            // Selection outline — always allocated, hidden if not selected
            let outline_id = tree.add_panel(
                self.viewport_clip_id,
                flag_x - 1.0,
                flag_y - 1.0,
                flag_w + 2.0,
                flag_h + 2.0,
                UIStyle {
                    bg_color: color::MARKER_SELECTED_OUTLINE,
                    ..UIStyle::default()
                },
            );
            if !is_selected || !in_view {
                tree.set_visible(outline_id, false);
            }
            self.marker_node_ids.push(outline_id);

            // Label — always allocated, hidden if empty or off-screen
            let label_x = flag_x + flag_w + 2.0;
            let label_y = flag_y + (flag_h - color::MARKER_LABEL_HEIGHT) * 0.5;
            let label_id = tree.add_label(
                self.viewport_clip_id,
                label_x,
                label_y,
                color::MARKER_LABEL_WIDTH,
                color::MARKER_LABEL_HEIGHT,
                if marker.name.is_empty() {
                    ""
                } else {
                    &marker.name
                },
                UIStyle {
                    bg_color: color::MARKER_LABEL_BG,
                    text_color: mc,
                    font_size: RULER_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            if marker.name.is_empty() || !in_view {
                tree.set_visible(label_id, false);
            }
            self.marker_node_ids.push(label_id);

            self.marker_groups.push(MarkerNodeGroup {
                flag_id,
                outline_id,
                label_id,
            });
        }
    }

    /// B13 — plain-text "bar.beat   len bar.beat" readout for the clip
    /// currently being moved/trimmed. Display-only chrome; no node persists
    /// across frames (added fresh, like the other build_* elements above,
    /// only when a gesture is in flight — skipped entirely otherwise).
    /// Styling (colors, layout polish) is explicitly deferred to
    /// `UI_CRAFT_AND_MOTION_PLAN.md` — this reuses existing plain label/bg
    /// tokens, no new design decisions.
    ///
    /// Anchored in the ruler strip (like the insert-cursor marker), NOT
    /// inside the tracks lane: per-layer clip content is painted as an
    /// opaque bitmap OUTSIDE the UITree ("Clips: painted into per-layer
    /// bitmap, not UITree nodes" — see `build()`'s comments), composited
    /// over the tracks area after the tree draw pass, so a Label node placed
    /// inside a lane's rect is invisible under that bitmap (verified via a
    /// ui-snap PNG: the label existed in the tree dump with the right text
    /// and the VISIBLE flag, but zero trace of it in the rendered pixels).
    /// The ruler is never bitmap-painted, so chrome anchored there is the
    /// only placement proven to actually show up.
    pub(super) fn build_drag_readout(&mut self, tree: &mut UITree) {
        let Some((position, duration, _layer_index)) = self.drag_readout else {
            self.drag_readout_label_id = None;
            return;
        };

        const READOUT_W: f32 = 160.0;
        const READOUT_H: f32 = 16.0;

        // Geometry first — both read `&self` immutably, so they must resolve
        // before the mutable borrow of `drag_readout_cache` below (whose
        // returned `&str` stays borrowed through the `add_label` call).
        let x = self.beat_to_pixel(position);
        let y = self.ruler_rect.y + self.ruler_rect.height - READOUT_H;

        // The FORMATTED STRING is dirty-checked (`DragReadoutCache`) — this
        // call is cheap (a value comparison) every frame; `format!` itself
        // only runs when position/duration/time-signature actually changed.
        let beats_per_bar = self.beats_per_bar;
        let text = self.drag_readout_cache.text(position, duration, beats_per_bar);

        let id = tree.add_label(
            self.viewport_clip_id,
            x,
            y,
            READOUT_W,
            READOUT_H,
            text,
            UIStyle {
                bg_color: color::MARKER_LABEL_BG,
                text_color: color::TEXT_NORMAL,
                font_size: RULER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        self.drag_readout_label_id = Some(id);
    }

    // ── Update-in-place (Phase 1: horizontal scroll) ───────────

    /// Try to update ruler ticks, labels, markers, and export markers in-place
    /// for a horizontal-only scroll. Returns `true` if successful, `false` if
    /// a full rebuild is needed (count mismatch or never built).
    pub fn try_update_horizontal_scroll(&mut self, tree: &mut UITree) -> bool {
        // Guard: must have been built at least once
        if self.ruler_tick_ids.is_empty() {
            return false;
        }

        // ── Recompute ruler parameters (same logic as build_ruler) ──

        let (min_beat, max_beat) = self.visible_beat_range();
        let bpb = self.beats_per_bar as f32;
        let ppb = self.mapper.pixels_per_beat();
        let subdiv = self.grid_subdivision();

        let tick_step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        const MIN_LABEL_SPACING: f32 = 50.0;
        let label_step: f32 = if ppb >= MIN_LABEL_SPACING {
            1.0
        } else if bpb * ppb >= MIN_LABEL_SPACING {
            bpb
        } else {
            let bar_px = bpb * ppb;
            let mut n_bars = 2.0_f32;
            while n_bars * bar_px < MIN_LABEL_SPACING && n_bars <= 1024.0 {
                n_bars *= 2.0;
            }
            bpb * n_bars
        };

        let bar_skip = self.bar_skip();
        let ruler_bottom = self.ruler_rect.y + self.ruler_rect.height;
        let start = (min_beat / tick_step).floor() * tick_step;
        let label_y = self.ruler_rect.y + 2.0;

        // ── Count ticks and labels, collect update data ──

        let mut tick_count = 0usize;
        let mut label_count = 0usize;
        let mut beat = start;

        // First pass: count only (to compare with existing)
        while beat <= max_beat && tick_count < MAX_RULER_TICKS {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

                tick_count += 1;

                if (beat % label_step).abs() < 0.001
                    && !self
                        .markers
                        .iter()
                        .any(|m| (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty())
                {
                    label_count += 1;
                }
            }
            beat += tick_step;
        }

        // Count mismatch → fallback to full rebuild
        if tick_count != self.ruler_tick_ids.len() || label_count != self.ruler_label_ids.len() {
            return false;
        }

        // Marker count changed → fallback
        if self.marker_groups.len() != self.markers.len() {
            return false;
        }

        // ── Second pass: update tick and label nodes in-place ──

        let mut tick_idx = 0usize;
        let mut label_idx = 0usize;
        beat = start;

        while beat <= max_beat && tick_idx < tick_count {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

                let is_beat = (beat % 1.0).abs() < 0.001;
                let is_label_beat = (beat % label_step).abs() < 0.001;

                let tick_h = if is_label_beat && is_bar {
                    RULER_BAR_TICK_H + 4.0
                } else if is_bar {
                    RULER_BAR_TICK_H
                } else if is_beat {
                    RULER_BEAT_TICK_H
                } else {
                    4.0
                };

                let tick_color = if is_label_beat && is_bar {
                    color::TEXT_NORMAL
                } else if is_bar {
                    color::TEXT_SUBTLE
                } else {
                    color::TEXT_FAINT
                };

                let id = self.ruler_tick_ids[tick_idx];
                tree.set_bounds(
                    id,
                    Rect::new(px, ruler_bottom - tick_h, RULER_TICK_W, tick_h),
                );
                tree.set_style(
                    id,
                    UIStyle {
                        bg_color: tick_color,
                        ..UIStyle::default()
                    },
                );
                tick_idx += 1;

                // Update label
                let has_marker_at_beat = self
                    .markers
                    .iter()
                    .any(|m| (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty());
                if is_label_beat && !has_marker_at_beat && label_idx < label_count {
                    let bar_num = (beat / bpb).floor() as i32 + 1;
                    let beat_in_bar = ((beat % bpb) + 0.001).floor() as i32 + 1;
                    let label = if is_bar {
                        format!("{}", bar_num)
                    } else {
                        format!("{}.{}", bar_num, beat_in_bar)
                    };

                    let lid = self.ruler_label_ids[label_idx];
                    tree.set_bounds(
                        lid,
                        Rect::new(px + 2.0, label_y, RULER_LABEL_W, RULER_LABEL_H),
                    );
                    tree.set_text(lid, &label);
                    tree.set_style(
                        lid,
                        UIStyle {
                            text_color: if is_bar {
                                color::TEXT_NORMAL
                            } else {
                                color::TEXT_DIMMED
                            },
                            font_size: RULER_FONT_SIZE,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );
                    label_idx += 1;
                }
            }
            beat += tick_step;
        }

        // ── Update export markers in-place ──

        if let (Some(export_in_marker_id), Some(export_range_id), Some(export_out_marker_id)) = (
            self.export_in_marker_id,
            self.export_range_id,
            self.export_out_marker_id,
        ) {
            let marker_w = 2.0;
            let marker_h = self.ruler_rect.height + self.tracks_rect.height;
            let enabled = self.export_range_enabled;
            let has_out = self.export_out_beat > self.export_in_beat;

            let in_px = self.beat_to_pixel(self.export_in_beat);
            let in_vis =
                enabled && in_px >= self.tracks_rect.x && in_px <= self.tracks_rect.x_max();
            tree.set_visible(export_in_marker_id, in_vis);
            if in_vis {
                tree.set_bounds(
                    export_in_marker_id,
                    Rect::new(
                        in_px - marker_w * 0.5,
                        self.ruler_rect.y,
                        marker_w,
                        marker_h,
                    ),
                );
            }

            let out_px = self.beat_to_pixel(self.export_out_beat);
            let range_left = in_px.max(self.tracks_rect.x);
            let range_right = out_px.min(self.tracks_rect.x_max());
            let range_w = (range_right - range_left).max(0.0);
            let range_vis = enabled && has_out && range_w > 0.0;
            tree.set_visible(export_range_id, range_vis);
            if range_vis {
                tree.set_bounds(
                    export_range_id,
                    Rect::new(
                        range_left,
                        self.tracks_rect.y,
                        range_w,
                        self.tracks_rect.height,
                    ),
                );
            }

            let out_vis = enabled
                && has_out
                && out_px >= self.tracks_rect.x
                && out_px <= self.tracks_rect.x_max();
            tree.set_visible(export_out_marker_id, out_vis);
            if out_vis {
                tree.set_bounds(
                    export_out_marker_id,
                    Rect::new(
                        out_px - marker_w * 0.5,
                        self.ruler_rect.y,
                        marker_w,
                        marker_h,
                    ),
                );
            }
        }

        // ── Update timeline markers in-place ──

        let flag_w = color::MARKER_FLAG_WIDTH;
        let flag_h = color::MARKER_FLAG_HEIGHT;

        for (i, marker) in self.markers.iter().enumerate() {
            let group = &self.marker_groups[i];
            // Same geometry source as build + hit-test (marker_flag_rect).
            let flag = self.marker_flag_rect(marker.beat);
            let flag_x = flag.x;
            let flag_y = flag.y;
            let px = self.beat_to_pixel(marker.beat);
            let in_view =
                px >= self.tracks_rect.x - flag_w && px <= self.tracks_rect.x_max() + flag_w;

            let mc = color::marker_color_to_color32(marker.color);
            let is_selected = self.selected_marker_ids.contains(&marker.id);

            // Flag
            tree.set_visible(group.flag_id, in_view);
            if in_view {
                let flag_color = if is_selected {
                    color::lighten(mc, 40)
                } else {
                    mc
                };
                tree.set_bounds(
                    group.flag_id,
                    Rect::new(flag_x, flag_y, flag_w, flag_h),
                );
                tree.set_style(
                    group.flag_id,
                    UIStyle {
                        bg_color: flag_color,
                        ..UIStyle::default()
                    },
                );
            }

            // Outline
            tree.set_visible(group.outline_id, in_view && is_selected);
            if in_view && is_selected {
                tree.set_bounds(
                    group.outline_id,
                    Rect::new(flag_x - 1.0, flag_y - 1.0, flag_w + 2.0, flag_h + 2.0),
                );
            }

            // Label
            let has_name = !marker.name.is_empty();
            tree.set_visible(group.label_id, in_view && has_name);
            if in_view && has_name {
                let label_x = flag_x + flag_w + 2.0;
                let label_y_m = flag_y + (flag_h - color::MARKER_LABEL_HEIGHT) * 0.5;
                tree.set_bounds(
                    group.label_id,
                    Rect::new(
                        label_x,
                        label_y_m,
                        color::MARKER_LABEL_WIDTH,
                        color::MARKER_LABEL_HEIGHT,
                    ),
                );
            }
        }

        // ── Update insert cursor ──
        self.sync_insert_cursor_ruler(tree);

        true
    }

    // ── Update-in-place (Phase 2: vertical scroll) ─────────────

    /// Try to update track background Y positions in-place for vertical scroll.
    /// Returns `true` if successful, `false` if full rebuild needed.
    pub fn try_update_vertical_scroll(&mut self, tree: &mut UITree) -> bool {
        // Guard: must match current track count
        if self.track_bg_groups.len() != self.tracks.len() || self.track_bg_groups.is_empty() {
            return false;
        }

        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;
        let tr_x = tr.x;
        let tr_w = tr.width;

        for (i, track) in self.tracks.iter().enumerate() {
            let group = &self.track_bg_groups[i];
            let y = self.track_y(i);
            let h = self.mapper.get_layer_height(i);

            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            let visible = clamped_h > 0.0 && y + h >= tr_top && y <= tr_bottom;

            // Background
            tree.set_visible(group.bg_id, visible);
            if visible {
                tree.set_bounds(
                    group.bg_id,
                    Rect::new(tr_x, clamped_y, tr_w, clamped_h),
                );
            }

            // Separator
            let sep_h = if track.is_group {
                color::GROUP_SEPARATOR_HEIGHT
            } else {
                color::TRACK_SEPARATOR_HEIGHT
            };
            let sep_y = y + h - sep_h;
            let sep_vis = visible && sep_y + sep_h > tr_top && sep_y < tr_bottom;
            tree.set_visible(group.separator_id, sep_vis);
            if sep_vis {
                tree.set_bounds(
                    group.separator_id,
                    Rect::new(
                        tr_x,
                        sep_y.max(tr_top),
                        tr_w,
                        (sep_y + sep_h).min(tr_bottom) - sep_y.max(tr_top),
                    ),
                );
            }
        }

        true
    }
}
