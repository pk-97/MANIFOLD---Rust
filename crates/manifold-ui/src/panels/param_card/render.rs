//! Surface builders/renderers for [`ParamCardPanel`] (P-S4 split of
//! `param_card.rs`) — `configure`, height computation, `build`/`build_effect`/
//! `build_generator`, and the `sync_values*` family.

use super::*;

/// The D3 relight-knob rows' colors when the "3D Shading" toggle is off —
/// desaturated off the grey ramp instead of the blue accent, so the greyed
/// state reads as visually distinct from an armed slider (no-conditionally-
/// visible-ui: still interactive, just not the "live" look).
fn relight_disabled_slider_colors() -> SliderColors {
    SliderColors {
        track: color::SLIDER_TRACK_C32,
        track_hover: color::SLIDER_TRACK_C32,
        track_pressed: color::SLIDER_TRACK_C32,
        fill: color::BG_3_HOVER,
        thumb: color::TEXT_DIMMED_C32,
        text: color::TEXT_DIMMED_C32,
    }
}

/// Packed right-aligned positions for the 0–4 header modulation badges.
/// In display order [MOD, ABL, ENV, DRV]; `None` for a hidden badge.
/// `name_right` is the right edge of the name cell (left edge of the badge
/// block, or the toggle gap when no badge shows) — what `name_w` clips to.
struct BadgeLayout {
    mod_x: Option<f32>,
    abl_x: Option<f32>,
    env_x: Option<f32>,
    drv_x: Option<f32>,
    aud_x: Option<f32>,
    name_right: f32,
}

/// Lay the visible header badges out CENTERED in the region between the name's
/// left edge (`content_left`) and the toggle. Packing only the active ones keeps
/// the cluster tight; centering keeps it clear of the ON/OFF toggle so the
/// badges don't read as another button. The name cell clips to `name_right`
/// (just before the cluster's left edge).
fn effect_badge_layout(
    content_left: f32,
    toggle_x: f32,
    show_mod: bool,
    show_abl: bool,
    show_env: bool,
    show_drv: bool,
    show_aud: bool,
) -> BadgeLayout {
    let shows = [show_mod, show_abl, show_env, show_drv, show_aud];
    let count = shows.iter().filter(|s| **s).count();
    let region_left = content_left;
    let region_right = toggle_x - GAP;
    let block_w = if count == 0 {
        0.0
    } else {
        count as f32 * BADGE_W + (count as f32 - 1.0) * GAP
    };
    // Centre the block in the region; clamp so it never runs under the toggle nor
    // off the left edge.
    let centered = (region_left + region_right) * 0.5 - block_w * 0.5;
    let block_left = centered.clamp(region_left, (region_right - block_w).max(region_left));
    let mut xs: [Option<f32>; 5] = [None; 5];
    let mut cursor = block_left;
    for (i, show) in shows.iter().enumerate() {
        if *show {
            xs[i] = Some(cursor);
            cursor += BADGE_W + GAP;
        }
    }
    BadgeLayout {
        mod_x: xs[0],
        abl_x: xs[1],
        env_x: xs[2],
        drv_x: xs[3],
        aud_x: xs[4],
        name_right: if count == 0 { region_right } else { block_left - GAP },
    }
}

/// D17 "spawn pop" geometry: the card's outer frame rect scaled by `s` about
/// its own center — `(x, y)` is the card's UNSCALED top-left, `(w, h)` its
/// UNSCALED width/height. A no-op (returns the exact input rect) once `s`
/// settles at `1.0`, so a settled card's geometry is bit-identical to the
/// pre-motion path.
fn scaled_card_rect(x: f32, y: f32, w: f32, h: f32, s: f32) -> Rect {
    if (s - 1.0).abs() < 0.0005 {
        return Rect::new(x, y, w, h);
    }
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    Rect::new(cx - w * 0.5 * s, cy - h * 0.5 * s, w * s, h * s)
}


impl ParamCardPanel {
    /// Configure from card metadata. Call before [`build`](Self::build).
    ///
    /// Sets `kind` from the config and populates every data-derived field for
    /// both shells (effect identity/badges + generator string params), so the
    /// same call serves either kind. The owning `layer_id` is NOT touched here
    /// — it is set independently via [`set_layer_id`](Self::set_layer_id)
    /// before configure (the generator config doesn't carry it).
    pub fn configure(&mut self, config: &ParamSurface) {
        self.kind = config.kind;
        self.effect_index = config.effect_index;
        self.effect_id = config.effect_id.clone();
        self.name = config.title.clone();
        self.enabled = config.enabled;
        self.relight = config.relight;
        self.is_collapsed = config.collapsed;
        self.sync_collapse_anim();
        self.supports_envelopes = config.supports_envelopes;
        self.rows = config.rows.clone();
        self.string_param_info = config.string_params.clone();

        let n = config.rows.len();
        // BUG-313: rebuild the id→row-index join map from the same rows being
        // rendered, so the per-frame value sync joins by id (never position).
        // Duplicate ids would silently corrupt the join (last-wins) — a built
        // manifest is dup-free, but assert it here where the map is formed.
        self.row_id_index.clear();
        self.row_id_index.reserve(n);
        for (i, row) in config.rows.iter().enumerate() {
            let prev = self.row_id_index.insert(row.id.to_string(), i);
            debug_assert!(
                prev.is_none(),
                "BUG-313: duplicate param id {:?} in card rows — the id-join map would corrupt",
                row.id,
            );
        }
        self.row_value_synced = vec![false; n];
        self.state = ParamCardState::new(n);
        self.state.has_drv = config.has_drv();
        self.state.has_env = config.has_env();
        self.state.has_abl = config.has_abl();
        self.state.has_graph_mod = config.has_graph_mod;
        let rows_mod: Vec<RowMod> = config.rows.iter().map(|r| r.modulation.clone()).collect();
        self.state.mod_state.sync_from_config(n, &rows_mod);
        self.state.mod_state.sync_audio(n, &config.audio);
        // AUD badge aggregate: any param has an armed audio modulation (parallels
        // has_drv / has_env). Derived after sync_audio populates audio_active.
        self.state.has_audio = self.state.mod_state.audio_active.iter().any(|&a| a);
        self.osc_addresses = config
            .rows
            .iter()
            .map(|r| r.mapping.osc_address.clone())
            .collect();
        self.copied_flash.clear();
        self.row_host.slider_ids = vec![None; n];
        self.row_host.slider_resets = vec![None; n];
        self.base_values = vec![0.0; n];
        self.row_host.row_catcher_ids = vec![None; n];
        self.row_host.driver_btn_ids = vec![None; n];
        self.row_host.envelope_btn_ids = vec![None; n];
        self.row_host.driver_config_ids = Vec::new();
        self.row_host.driver_config_ids.resize_with(n, || None);
        self.row_host.audio_btn_ids = vec![None; n];
        self.row_host.audio_configs = Vec::new();
        self.row_host.audio_configs.resize_with(n, || None);
        self.row_host.audio_trigger_mode_badge_ids = vec![None; n];
        self.row_host.target_ids = Vec::new();
        self.row_host.target_ids.resize_with(n, || None);
        self.row_host.envelope_config_ids = Vec::new();
        self.row_host.envelope_config_ids.resize_with(n, || None);
        self.row_host.trim_ids = Vec::new();
        self.row_host.trim_ids.resize_with(n, || None);
        self.row_host.ableton_trim_ids = Vec::new();
        self.row_host.ableton_trim_ids.resize_with(n, || None);
        self.row_host.audio_trim_ids = Vec::new();
        self.row_host.audio_trim_ids.resize_with(n, || None);
        self.row_host.ableton_config_ids = Vec::new();
        self.row_host.ableton_config_ids.resize_with(n, || None);
        // Preserve the per-param tab choice across rebuilds (UI state); only grow
        // for new params. resolve_active_tab clamps stale choices at build time.
        self.mod_active_tab.resize(n, ModTab::Driver);
        self.mod_active_tab.truncate(n);
        // P1 drawer tween targets. Preserve existing tweens across the rebuild (a
        // mid-flight tween must not reset), grow for new params. Then point each at
        // its settled drawer height: a *new* param snaps so it never stalls
        // half-open; an existing param eases (set_target no-ops when the target is
        // unchanged, so the per-frame rebuild that drives the tween doesn't reset
        // it). Targets are read into a temp first — `row_drawer_height` borrows
        // `&self` while the loop needs `&mut self.drawer_height_anim`.
        //
        // Both contexts ease identically since
        // `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 (D4): the editor's
        // `UIRoot` now ticks its inspector every frame it presents
        // (`UIRoot::tick_inspector`), so an Author card's tween advances the
        // same as a Perform card's — the old never-ticked-Author snap
        // workaround is gone because the workaround is.
        let prev_anim_len = self.drawer_height_anim.len();
        self.drawer_height_anim
            .resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS));
        self.drawer_height_anim.truncate(n);
        let drawer_targets: Vec<f32> = (0..n).map(|i| self.row_drawer_height(i)).collect();
        for (i, &target) in drawer_targets.iter().enumerate() {
            if i < prev_anim_len {
                self.drawer_height_anim[i].set_target(target);
            } else {
                self.drawer_height_anim[i].snap(target);
            }
        }
        self.row_host.mod_tab_ids = vec![Vec::new(); n];
        // Ink x-position targets are only knowable once the tab strip is laid
        // out (build time, not here) — resize only; `sync_mod_tab_ink` sets
        // targets per-row after `build_param_row` returns.
        self.mod_tab_ink.resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS));
        self.mod_tab_ink.truncate(n);
        self.row_host.toggle_ids = Vec::new();
        self.row_host.toggle_ids.resize_with(n, || None);
        self.row_host.mapping_chevron_ids = vec![None; n];
        self.string_param_btn_ids = vec![None; config.string_params.len()];
        self.param_cache = vec![f32::NAN; n];
        self.toggle_cache = vec![false; n];
        self.label_cache = vec![None; n];
        self.value_flash.resize_with(n, Transient::default);
        self.value_flash.truncate(n);
        self.value_snapback
            .resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS).with_curve(crate::anim::Curve::Snap));
        self.value_snapback.truncate(n);
    }

    pub fn compute_height(&self) -> f32 {
        match self.kind {
            ParamCardKind::Effect => self.compute_height_effect(),
            ParamCardKind::Generator => self.compute_height_generator(),
        }
    }

    fn compute_height_effect(&self) -> f32 {
        let h = BORDER_W * 2.0 + HEADER_HEIGHT + self.effect_body_natural_height() * self.collapse_frac();
        h + CARD_BOTTOM_MARGIN
    }

    /// The effect card's param-row block height at full expansion (frac = 1),
    /// including each row's own P1 drawer contribution. `compute_height_effect`
    /// scales this by `collapse_frac()`; `build_effect` sizes the animated
    /// clip-reveal region to it while `collapse_anim` is mid-flight.
    ///
    /// BUG-108: walks `section_runs()` — mirroring `build_effect`'s own draw
    /// loop exactly — instead of summing `rows` linearly. A linear sum
    /// is blind to the D5 section-header bar every section run draws
    /// (`build_section_header`, `ROW_HEIGHT + ROW_SPACING`) and to a folded
    /// section's rows drawing nothing at all; either one made this height
    /// shorter than what `build_effect` actually painted, so the "+ Add
    /// Effect" button anchored below it (`layer_column_height`) landed
    /// mid-card instead of below the last drawn row.
    fn effect_body_natural_height(&self) -> f32 {
        // The relight block below always draws when expanded (P5b), so the
        // body is never truly empty anymore even with zero regular params.
        let mut h = HEADER_BODY_GAP;
        for (start, len, section) in self.section_runs() {
            if let Some(name) = &section {
                // Section header bar — drawn even when every row in the run
                // is folded away.
                h += ROW_HEIGHT + ROW_SPACING;
                let folded = self.section_folded.get(name).copied().unwrap_or(false);
                if folded {
                    continue;
                }
            }
            for i in start..start + len {
                // Hidden params consume zero vertical space.
                if !self.rows[i].value.exposed {
                    continue;
                }
                h += ROW_HEIGHT + ROW_SPACING;
                // A plain toggle never gets a drawer (nothing to modulate) — zero
                // lane, zero height, unconditionally. `is_trigger` and ordinary
                // sliders both go through the general `active_mod_tabs`-driven
                // height (`animated_drawer_height` already handles "no active
                // config → 0" on its own; is_trigger only ever has Audio active,
                // per D5b). `is_trigger_gate` is ALSO an `is_toggle` row (D6) but
                // reaches its own `AudioTrigger` tab through the same path.
                if !self.rows[i].spec.is_toggle || self.rows[i].spec.is_trigger_gate {
                    h += self.animated_drawer_height(i);
                }
            }
        }
        h + self.relight_block_height()
    }

    /// Fixed height of the always-visible D3/D4 "3D Shading" block: a
    /// section label + the six knob rows + the Height From row. Drawn
    /// (greyed when off, never hidden — no-conditionally-visible-ui)
    /// regardless of whether the card has any regular params
    /// (`docs/DEPTH_RELIGHT_DESIGN.md` P5b).
    fn relight_block_height(&self) -> f32 {
        // Feature disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // the "3D Shading" block is not drawn, so it contributes no height.
        if !RELIGHT_FEATURE_ENABLED {
            return 0.0;
        }
        const RELIGHT_ROW_COUNT: f32 = 1.0 + 6.0 + 1.0; // label + 6 knobs + Height From
        RELIGHT_ROW_COUNT * (ROW_HEIGHT + ROW_SPACING)
    }

    /// BUG-108: same section-run walk as `effect_body_natural_height` — see
    /// its doc comment. `build_generator` draws section headers identically
    /// to `build_effect` (same `build_section_header` call, same fold-skip),
    /// so this needs the same fix.
    fn compute_height_generator(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed {
            // Always true now — the relight block below always draws when
            // expanded (P5b), so the body is never truly empty.
            h += HEADER_BODY_GAP;
            for (start, len, section) in self.section_runs() {
                if let Some(name) = &section {
                    h += ROW_HEIGHT + ROW_SPACING;
                    let folded = self.section_folded.get(name).copied().unwrap_or(false);
                    if folded {
                        continue;
                    }
                }
                for i in start..start + len {
                    h += ROW_HEIGHT + ROW_SPACING;
                    // Same rule as `effect_body_natural_height`: only a plain
                    // toggle forces zero drawer height. `is_trigger` reaches the
                    // audio-mod drawer (D5b), `is_trigger_gate` reaches the
                    // audio-TRIGGER-mod drawer (D6) — both via the same general
                    // height path every slider row uses.
                    if !self.rows[i].spec.is_toggle || self.rows[i].spec.is_trigger_gate {
                        h += self.animated_drawer_height(i);
                    }
                }
            }
            // String param rows (text fields)
            for _ in &self.string_param_info {
                h += ROW_HEIGHT + ROW_SPACING;
            }
            h += self.relight_block_height();
            h += PADDING;
        }
        h + CARD_BOTTOM_MARGIN
    }

    /// §6b — set compact mode (hide all modulation config drawers on this card).
    /// Driven by the inspector's global "hide mod settings" toggle.
    pub fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
    }

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.row_host.row_index.clear();
        match self.kind {
            ParamCardKind::Effect => self.build_effect(tree, rect),
            ParamCardKind::Generator => self.build_generator(tree, rect),
        }
    }

    /// The generator card as a host `View`: frame + a declarative header
    /// (`[name | Change | cog | chevron]`, right-to-left). The cog's three dots
    /// are added imperatively into the keyed cog button after build (absolute
    /// decoration that doesn't map to flow layout); in Author the cog button is a
    /// reserved transparent slot so the rest stays put.
    fn generator_card_view(&self, border_color: Color32) -> View {
        let change_style = UIStyle {
            bg_color: color::CONFIG_BG_C32,
            hover_bg_color: color::GEN_CARD_HEADER_HOVER_C32,
            pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size: FONT_SIZE,
            corner_radius: color::SMALL_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        };
        let gap = || View::panel().w(Sizing::Fixed(GAP)).fill_h();
        let cog = if self.context == CardContext::Perform {
            View::button("")
                .w(Sizing::Fixed(COG_W))
                .fill_h()
                .style(UIStyle {
                    bg_color: Color32::TRANSPARENT,
                    hover_bg_color: color::HOVER_OVERLAY,
                    pressed_bg_color: color::PRESS_OVERLAY,
                    ..UIStyle::default()
                })
                .inert()
                .key(KEY_COG)
        } else {
            View::panel().w(Sizing::Fixed(COG_W)).fill_h()
        };
        let header = View::row(0.0)
            .fill_w()
            .h(Sizing::Fixed(HEADER_HEIGHT))
            .bg(self.header_bg())
            .radius(CORNER_RADIUS - BORDER_W)
            .interactive()
            .inert()
            // §14.5 D — one right gutter: trailing controls right-align to
            // `inner_right - PADDING`, same as the effect header and the param
            // rows' value/mod-icon lane (was r: 0, flush to the inner edge).
            .pad(Pad { l: PADDING, t: 0.0, r: PADDING, b: 0.0 })
            .cross_align(Align::Center)
            .key(KEY_HEADER_BG)
            .child(
                View::label(self.name.as_str())
                    .fill_w()
                    .fill_h()
                    .font(HEADER_FONT_SIZE)
                    .text_color(self.header_name_color())
                    .align_text(TextAlign::Left)
                    .interactive()
                    .inert()
                    .key(KEY_NAME),
            )
            .child(gap())
            .child(
                View::button("Change")
                    .w(Sizing::Fixed(CHANGE_BTN_W))
                    .h(Sizing::Fixed(CHANGE_BTN_H))
                    .style(change_style)
                    .inert()
                    .key(KEY_CHANGE),
            )
            // "3D Shading" toggle (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) and its
            // leading gap — hidden entirely while the feature is disabled app-wide
            // (`manifold_foundation::RELIGHT_FEATURE_ENABLED`). `Option<View>` is
            // `IntoIterator`, so `children` adds 0 or 1 views.
            .children(RELIGHT_FEATURE_ENABLED.then(gap))
            .children(RELIGHT_FEATURE_ENABLED.then(|| {
                View::button("3D")
                    .w(Sizing::Fixed(RELIGHT_W))
                    .fill_h()
                    .style(toggle_btn_style(self.relight.enabled))
                    .inert()
                    .key(KEY_RELIGHT)
            }))
            .child(gap())
            .child(cog)
            .child(
                // P2 "caret rotate": same single-glyph + rotation technique as
                // the effect header's chevron (`chevron_angle`'s doc comment) —
                // generator cards' `collapse_anim` always snaps rather than
                // eases, so this reads as an instant flip here, same as before.
                View::button("\u{25BC}")
                    .w(Sizing::Fixed(CHEVRON_W))
                    .fill_h()
                    .style(UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Center,
                        transform: Some(Affine2::rotate(self.chevron_angle())),
                        ..UIStyle::default()
                    })
                    .inert()
                    .key(KEY_CHEVRON),
            );

        View::panel()
            .fill()
            .bg(border_color)
            .radius(CORNER_RADIUS)
            .interactive()
            .inert()
            .key(KEY_BORDER)
            .identity(self.identity_key())
            .pad(Pad::all(BORDER_W))
            .child(
                View::panel()
                    .fill()
                    .bg(self.base_inner_bg())
                    .radius(CORNER_RADIUS - BORDER_W)
                    .interactive()
                    .inert()
                    .key(KEY_INNER)
                    .child(header),
            )
    }

    /// Add the cog's three triangle dots as children of the keyed cog button.
    fn add_cog_dots(&self, tree: &mut UITree, cog_btn_id: NodeId) {
        let b = tree.get_bounds(cog_btn_id);
        let dot: f32 = 3.0;
        let dot_style = UIStyle {
            bg_color: color::TEXT_DIMMED_C32,
            corner_radius: dot * 0.5,
            ..UIStyle::default()
        };
        let cx = b.x + COG_W * 0.5;
        let cy = b.y + HEADER_HEIGHT * 0.5;
        let v_offset = 3.5;
        let h_offset = 4.0;
        let positions = [
            (cx - dot * 0.5, cy - v_offset - dot * 0.5),
            (cx - h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
            (cx + h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
        ];
        for (px, py) in positions {
            tree.add_panel(Some(cog_btn_id), px, py, dot, dot, dot_style);
        }
    }

    /// The effect card frame on the host: border + inner bg + the header
    /// background (tinted pink when the card carries a per-card graph override).
    /// The header *contents* (drag handle, name, badges, toggle, chevron, cog)
    /// are still built imperatively into this header bg.
    fn effect_frame_view(&self, border_color: Color32) -> View {
        let header_bg = self.header_bg();
        View::panel()
            .fill()
            .bg(border_color)
            .radius(CORNER_RADIUS)
            .interactive()
            .inert()
            .key(KEY_BORDER)
            .identity(self.identity_key())
            .pad(Pad::all(BORDER_W))
            .child(
                View::panel()
                    .fill()
                    .bg(self.base_inner_bg())
                    .radius(CORNER_RADIUS - BORDER_W)
                    .interactive()
                    .inert()
                    .key(KEY_INNER)
                    .child(self.effect_header_row(header_bg)),
            )
    }

    /// The effect header structure as a `View`: `[drag? | name-clip | toggle |
    /// chevron | cog?]` right-to-left. The badges, drag bars, and cog dots are
    /// added imperatively afterwards (see `build_effect_header`); the name-clip
    /// is laid `Fill` here and shrunk to leave room for active badges by the
    /// in-place re-pack, so badge behaviour is unchanged.
    fn effect_header_row(&self, header_bg: Color32) -> View {
        let author = self.context == CardContext::Author;
        let transparent_btn = |hover: Color32, pressed: Color32| UIStyle {
            bg_color: Color32::TRANSPARENT,
            hover_bg_color: hover,
            pressed_bg_color: pressed,
            ..UIStyle::default()
        };
        let mut row = View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(HEADER_HEIGHT))
            .bg(header_bg)
            .radius(CORNER_RADIUS - BORDER_W)
            .interactive()
            .inert()
            .pad(Pad { l: PADDING, t: 0.0, r: PADDING, b: 0.0 })
            .cross_align(Align::Center)
            .key(KEY_HEADER_BG);
        if !author {
            row = row.child(
                View::button("")
                    .fixed(DRAG_HANDLE_W, 16.0)
                    .style(transparent_btn(
                        color::DRAG_HANDLE_HOVER_BG_C32,
                        color::DRAG_HANDLE_BG_C32,
                    ))
                    .inert()
                    .key(KEY_DRAG),
            );
        }
        row = row
            .child(
                View::panel()
                    .clip()
                    .fill_w()
                    .h(Sizing::Fixed(16.0))
                    .key(KEY_NAME_CLIP)
                    .child(
                        View::label(self.name.as_str())
                            .fill_w()
                            .fill_h()
                            .font(HEADER_FONT_SIZE)
                            .text_color(self.header_name_color())
                            .align_text(TextAlign::Left)
                            .key(KEY_NAME),
                    ),
            )
            .child(
                View::button(if self.enabled { "ON" } else { "OFF" })
                    .fixed(TOGGLE_W, 16.0)
                    .style(toggle_btn_style(self.enabled))
                    .inert()
                    .key(KEY_TOGGLE),
            )
            // "3D Shading" toggle (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) — hidden
            // entirely while the feature is disabled app-wide
            // (`manifold_foundation::RELIGHT_FEATURE_ENABLED`).
            .children(RELIGHT_FEATURE_ENABLED.then(|| {
                View::button("3D")
                    .fixed(RELIGHT_W, 16.0)
                    .style(toggle_btn_style(self.relight.enabled))
                    .inert()
                    .key(KEY_RELIGHT)
            }));
        // Cog (or a reserved slot in Author) sits LEFT of the chevron so the
        // expand chevron is always the rightmost control — same trailing order as
        // the generator header (… · cog · ▾).
        // P2 "caret rotate": one down-pointing glyph (▼), rotated to ▶ via
        // `chevron_angle()`/`UIStyle.transform` instead of swapping glyphs —
        // see `chevron_angle`'s doc comment.
        let chevron = View::button("\u{25BC}")
            .fixed(CHEVRON_W, 16.0)
            .style(UIStyle {
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                transform: Some(Affine2::rotate(self.chevron_angle())),
                ..transparent_btn(color::HOVER_OVERLAY, color::PRESS_OVERLAY)
            })
            .inert()
            .key(KEY_CHEVRON);
        if !author {
            row.child(
                View::button("")
                    .fixed(COG_W, 16.0)
                    .style(transparent_btn(color::HOVER_OVERLAY, color::PRESS_OVERLAY))
                    .inert()
                    .key(KEY_COG),
            )
            .child(chevron)
        } else {
            row.child(View::panel().w(Sizing::Fixed(COG_W)).fill_h()).child(chevron)
        }
    }

    fn build_effect(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        // Stacking/hit-test position stays at the UNSCALED rect — only the
        // drawn geometry below pops; a card mid-pop must not jitter its
        // neighbors' reflow or its own drag-reorder hit test.
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        // Card frame (border + inner bg) on the host — interactive so clicks on
        // the edge / body select the card (resolved by id in `handle_click`).
        let border_color = self.base_border_color();
        let view = self.effect_frame_view(border_color);
        let h = self.compute_height() - CARD_BOTTOM_MARGIN;
        // D17 "spawn pop": scale the whole card about its own center (the
        // incoming `rect`'s own width/computed height — NOT `rect.height`,
        // which callers pass as a loose bounding box, e.g. tests build at a
        // fixed 300px regardless of the card's real height). Every child
        // node below is positioned from `inner` (`tree.get_bounds` on this
        // scaled frame), so the header/badges/rows pop as one rigid piece
        // with no separate per-child transform — see `spawn_scale`'s doc
        // comment.
        let frame_rect = scaled_card_rect(rect.x, rect.y, rect.width, h, self.spawn_scale.value());
        self.host.build(tree, &view, frame_rect);
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        let inner = tree.get_bounds(self.inner_bg_id.expect("frame built inner bg"));
        let inner_w = inner.width;
        let parent = self.inner_bg_id.expect("frame built inner bg");

        // Header contents (badges + decorations into the host-owned header).
        self.build_effect_header(tree, inner.x, inner.y, inner_w);

        // Param sliders — P2 "card collapse": `collapse_frac()` scales the
        // body's reserved height (see `compute_height_effect`); while
        // `collapse_anim` is mid-flight, the row block builds under a
        // `ClipRegion` sized to the CURRENT animated height (the same
        // top-down-reveal technique `build_param_row`'s per-row P1 drawer
        // tween uses — `param_slider_shared.rs`'s `drawer_parent`) so rows
        // never visually overflow the shrinking/growing card frame. A
        // settled card keeps the exact old behavior: skip entirely when
        // collapsed, build unclipped under `parent` when expanded.
        let frac = self.collapse_frac();
        // The relight rows below draw whenever expanded, regardless of
        // whether this card has any regular params — see the matching
        // comment in `build_generator`.
        if frac > 0.0 {
            let body_y = inner.y + HEADER_HEIGHT + HEADER_BODY_GAP;
            let sliders_parent = if self.collapse_anim.is_animating() {
                tree.add_node(
                    Some(parent),
                    Rect::new(inner.x, body_y, inner_w.max(1.0), (self.effect_body_natural_height() * frac).max(0.0)),
                    UINodeType::ClipRegion,
                    UIStyle::default(),
                    None,
                    UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
                )
            } else {
                parent
            };
            self.build_effect_sliders(tree, sliders_parent, inner.x, body_y, inner_w);
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_effect_header(&mut self, tree: &mut UITree, x: f32, y: f32, w: f32) {
        // Header background is host-owned (see `effect_frame_view`, tinted there
        // by `has_graph_mod`); the contents below nest under it.
        let header_bg_id = self.header_bg_id.expect("header bg built by host");

        // Layout (right-to-left for fixed elements). Badges pack flush against
        // the toggle — only the active ones take a slot — so the name cell is
        // as wide as possible and a lone badge never floats mid-header.
        // Trailing order (right→left): chevron (always rightmost), cog, toggle —
        // matches the host View child order in `effect_header_row`.
        let chevron_x = x + w - PADDING - CHEVRON_W;
        let cog_x = chevron_x - GAP - COG_W;
        let toggle_x = cog_x - GAP - TOGGLE_W;
        // Left edge of the name/badge region — after the drag handle (perform) or
        // at the padding (author, no drag handle).
        let content_left = x + PADDING
            + if self.context == CardContext::Author { 0.0 } else { DRAG_HANDLE_W + GAP };
        let badges = effect_badge_layout(
            content_left,
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
            self.state.has_audio,
        );
        let badge_park = toggle_x - GAP - BADGE_W;
        let mod_x = badges.mod_x.unwrap_or(badge_park);
        let abl_x = badges.abl_x.unwrap_or(badge_park);
        let env_x = badges.env_x.unwrap_or(badge_park);
        let drv_x = badges.drv_x.unwrap_or(badge_park);
        let aud_x = badges.aud_x.unwrap_or(badge_park);
        let elem_y = y + (HEADER_HEIGHT - 16.0) * 0.5;
        let badge_y = y + (HEADER_HEIGHT - BADGE_H) * 0.5;

        // The header structure (drag handle, name-clip + label, toggle, chevron,
        // cog) is host-built (see `effect_header_row`); resolve its ids by key.
        // The badges, the drag bars, and the cog dots below are the imperative
        // decorations layered on top.
        self.drag_icon_id = self.host.node_id_for_key(KEY_DRAG);
        self.name_clip_id = self.host.node_id_for_key(KEY_NAME_CLIP);
        self.name_label_id = self.host.node_id_for_key(KEY_NAME);
        self.toggle_btn_id = self.host.node_id_for_key(KEY_TOGGLE);
        self.relight_btn_id = self.host.node_id_for_key(KEY_RELIGHT);
        self.chevron_btn_id = self.host.node_id_for_key(KEY_CHEVRON);
        self.cog_btn_id = self.host.node_id_for_key(KEY_COG);

        // Drag-handle bars (3 horizontal lines) into the host drag button.
        if let Some(drag_icon_id) = self.drag_icon_id {
            let dh_x = x + PADDING;
            let bar_w: f32 = 10.0;
            let bar_h: f32 = 1.5;
            let bar_x = dh_x + (DRAG_HANDLE_W - bar_w) * 0.5;
            let bar_style = UIStyle {
                bg_color: color::TEXT_DIMMED_C32,
                ..UIStyle::default()
            };
            for i in 0..3 {
                let bar_y = elem_y + 3.5 + i as f32 * 3.5;
                tree.add_panel(Some(drag_icon_id), bar_x, bar_y, bar_w, bar_h, bar_style);
            }
        }

        // ABL badge — visibility synced from state.has_abl
        let show_abl = self.state.has_abl;
        let abl_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            abl_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ABL_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.abl_badge_bg_id = Some(abl_badge_bg_id);
        let abl_badge_text_id = tree.add_label(
            Some(abl_badge_bg_id),
            abl_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "ABL",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.abl_badge_text_id = Some(abl_badge_text_id);
        tree.set_visible(abl_badge_bg_id, show_abl);
        tree.set_visible(abl_badge_text_id, show_abl);

        // ENV badge — visibility synced from state.has_env
        let show_env = self.state.has_env;
        let env_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ENVELOPE_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.env_badge_bg_id = Some(env_badge_bg_id);
        let env_badge_text_id = tree.add_label(
            Some(env_badge_bg_id),
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "TRG",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.env_badge_text_id = Some(env_badge_text_id);
        tree.set_visible(env_badge_bg_id, show_env);
        tree.set_visible(env_badge_text_id, show_env);

        // DRV badge — visibility synced from state.has_drv
        let show_drv = self.state.has_drv;
        let drv_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::DRIVER_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.drv_badge_bg_id = Some(drv_badge_bg_id);
        let drv_badge_text_id = tree.add_label(
            Some(drv_badge_bg_id),
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "LFO",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.drv_badge_text_id = Some(drv_badge_text_id);
        tree.set_visible(drv_badge_bg_id, show_drv);
        tree.set_visible(drv_badge_text_id, show_drv);

        // MOD badge — pink chip indicating the card's graph topology
        // diverges from the catalog default.
        let show_mod = self.state.has_graph_mod;
        let mod_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            mod_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::MOD_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.mod_badge_bg_id = Some(mod_badge_bg_id);
        let mod_badge_text_id = tree.add_label(
            Some(mod_badge_bg_id),
            mod_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "MOD",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.mod_badge_text_id = Some(mod_badge_text_id);
        tree.set_visible(mod_badge_bg_id, show_mod);
        tree.set_visible(mod_badge_text_id, show_mod);

        // AUD badge — green chip, matching the audio "A" arm button; shows when
        // any param on the card has an armed audio modulation.
        let show_aud = self.state.has_audio;
        let aud_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            aud_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::AUDIO_TRIM_BAR_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.aud_badge_bg_id = Some(aud_badge_bg_id);
        let aud_badge_text_id = tree.add_label(
            Some(aud_badge_bg_id),
            aud_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "AUD",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.aud_badge_text_id = Some(aud_badge_text_id);
        tree.set_visible(aud_badge_bg_id, show_aud);
        tree.set_visible(aud_badge_text_id, show_aud);

        self.cached_has_env = show_env;
        self.cached_has_drv = show_drv;
        self.cached_has_abl = show_abl;
        self.cached_has_audio = show_aud;
        self.cached_has_graph_mod = show_mod;
        self.cached_enabled = self.enabled;

        // Cog dots (three in a triangle) into the host cog button.
        if let Some(cog_btn_id) = self.cog_btn_id {
            let dot: f32 = 3.0;
            let dot_style = UIStyle {
                bg_color: color::TEXT_DIMMED_C32,
                corner_radius: dot * 0.5,
                ..UIStyle::default()
            };
            let cx = cog_x + COG_W * 0.5;
            let cy = elem_y + 8.0;
            let v_offset = 3.5;
            let h_offset = 4.0;
            let positions = [
                (cx - dot * 0.5, cy - v_offset - dot * 0.5),
                (cx - h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
                (cx + h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
            ];
            for (px, py) in positions {
                tree.add_panel(Some(cog_btn_id), px, py, dot, dot, dot_style);
            }
        }

        // Shrink the host name-clip to leave room for the active badges and
        // settle the badge positions — the same in-place re-pack `sync` runs, so
        // badge behaviour is unchanged.
        self.reposition_effect_badges(tree);
    }

    /// Contiguous runs of `rows[..].section` — the D5 display-grouping
    /// unit (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): a run is a maximal
    /// span of consecutive rows sharing the same section value (`None`
    /// included — an unsectioned run renders with no header at all). A
    /// repeated section name after a gap is intentionally a SECOND run/header
    /// (display grouping only groups contiguous rows; forbidden move: do not
    /// reorder rows to force contiguity). Returns `(start_index, len,
    /// section)` triples covering `0..rows.len()` with no gaps.
    pub(crate) fn section_runs(&self) -> Vec<(usize, usize, Option<String>)> {
        let mut runs = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            let section = self.rows[i].spec.section.clone();
            let mut j = i + 1;
            while j < self.rows.len() && self.rows[j].spec.section == section {
                j += 1;
            }
            runs.push((i, j - i, section));
            i = j;
        }
        runs
    }

    /// Build one D5 section-header row: a clickable bar with a fold triangle,
    /// the section name (its own label node, so a UI-flow assertion can match
    /// the bare name exactly), and — when folded — a row-count chip. Returns
    /// the row's own clickable node id; the caller registers `(id, name)`
    /// into `section_header_ids` so `handle_click` can resolve a click back
    /// to the section without a second lookup. Fold state itself lives in
    /// `section_folded` (UI-local workspace state, not serialized — see its
    /// doc comment); this fn only reads `folded`, it does not toggle it.
    fn build_section_header(
        &mut self,
        tree: &mut UITree,
        parent: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        name: &str,
        folded: bool,
        row_count: usize,
        key_base: u64,
    ) -> NodeId {
        let header_id = tree.add_button_keyed(
            parent,
            x,
            y,
            w,
            ROW_HEIGHT,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
            "",
            key_base | ROW_ROLE_SECTION_HEADER,
        );
        let triangle_w = 16.0;
        let triangle = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
        tree.add_label(
            Some(header_id),
            x + GAP,
            y,
            triangle_w,
            ROW_HEIGHT,
            triangle,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        let count_w = if folded { 40.0 } else { 0.0 };
        tree.add_label(
            Some(header_id),
            x + GAP + triangle_w,
            y,
            (w - GAP * 2.0 - triangle_w - count_w).max(0.0),
            ROW_HEIGHT,
            name,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        if folded {
            tree.add_label(
                Some(header_id),
                x + w - GAP - count_w,
                y,
                count_w,
                ROW_HEIGHT,
                &format!("({row_count})"),
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: color::FONT_CAPTION,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );
        }
        header_id
    }

    /// Reset every per-index interactive-id slot for row `i` to "nothing
    /// built this frame" — called for a folded section's rows so a stale id
    /// from a PRIOR build (a different frame, a different `UITree`
    /// instance — two trees can mint numerically colliding ids) is never
    /// mistaken for a live widget in the CURRENT tree. Scoped to the D5
    /// fold-skip path only (the pre-existing `!exposed` skip is untouched —
    /// out of scope for this phase).
    fn clear_row_ids(&mut self, i: usize) {
        self.row_host.slider_ids[i] = None;
        self.row_host.slider_resets[i] = None;
        self.row_host.row_catcher_ids[i] = None;
        self.row_host.driver_btn_ids[i] = None;
        self.row_host.envelope_btn_ids[i] = None;
        self.row_host.driver_config_ids[i] = None;
        self.row_host.audio_btn_ids[i] = None;
        self.row_host.audio_configs[i] = None;
        self.row_host.audio_trigger_mode_badge_ids[i] = None;
        self.row_host.target_ids[i] = None;
        self.row_host.envelope_config_ids[i] = None;
        self.row_host.trim_ids[i] = None;
        self.row_host.ableton_trim_ids[i] = None;
        self.row_host.audio_trim_ids[i] = None;
        self.row_host.ableton_config_ids[i] = None;
        self.row_host.mapping_chevron_ids[i] = None;
        self.row_host.toggle_ids[i] = None;
        self.row_host.mod_tab_ids[i] = Vec::new();
    }

    fn build_effect_sliders(
        &mut self,
        tree: &mut UITree,
        parent: NodeId,
        x: f32,
        start_y: f32,
        w: f32,
    ) {
        let mut cy = start_y;
        // `author` gates both the chevron lane reservation and the glyph
        // draw + row-id scheme below — the lane only exists where the glyph
        // can appear (Author + mappable).
        let author = self.context == CardContext::Author;
        // Label column grows with the row so a wide inspector card gives the
        // param name more room (not just a longer track). Floored at the
        // default, so narrow timeline cards keep the timeline's width exactly.
        // Shared with `build_generator` via `row_geometry` (D2) so the two
        // builders' lane math can't drift from each other.
        let RowGeometry { label_width, slider_w } = row_geometry(w - PADDING * 2.0, author);

        self.row_host.section_header_ids.clear();
        let runs = self.section_runs();
        for (start, len, section) in runs {
            if let Some(name) = &section {
                let folded = self.section_folded.get(name).copied().unwrap_or(false);
                let header_id = self.build_section_header(
                    tree,
                    Some(parent),
                    x + PADDING,
                    cy,
                    w - PADDING * 2.0,
                    name,
                    folded,
                    len,
                    param_row_key_base(&self.rows[start].id),
                );
                self.row_host.section_header_ids.push((header_id, name.clone()));
                self.row_host.row_index.insert(tree.widget_of(header_id), start, RowRole::SectionHeader);
                cy += ROW_HEIGHT + ROW_SPACING;
                if folded {
                    // Folded run: no rows built for start..start+len. Clear
                    // every per-index id explicitly (`clear_row_ids`) rather
                    // than leaving stale ones from a prior build — a fold
                    // toggles at runtime (unlike `!exposed`, an authoring-time
                    // state), so a click on the now-hidden space must never
                    // resolve against a widget from a different frame's tree.
                    for i in start..start + len {
                        self.clear_row_ids(i);
                    }
                    continue;
                }
            }

        for i in start..start + len {
            // Hidden params: leave slider_ids[i] = None and skip widget
            // construction entirely. Slot-index semantics for any attached
            // driver/Ableton mapping/envelope are preserved.
            if !self.rows[i].value.exposed {
                continue;
            }
            let info = self.rows[i].clone();

            if info.spec.is_toggle || info.spec.is_trigger {
                // Toggle / Trigger row — shared builder (Task A of §8.4 P3b:
                // effect cards previously had no branch for this at all and
                // fell through to `build_param_row`, rendering a boolean/
                // fire-once param as a raw draggable slider). Same shared
                // core the generator card uses; effects gate the driver-
                // column reservation on `supports_envelopes` like their
                // slider rows do, so an `is_trigger` row's lone "A" button
                // still lands in the same column.
                let has_osc = self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some();
                let row = build_toggle_trigger_row(
                    tree,
                    Some(parent),
                    x + PADDING,
                    cy,
                    slider_w,
                    &info,
                    &self.state.mod_state,
                    i,
                    self.param_target(),
                    CONFIG_BTN_FONT_SIZE,
                    self.supports_envelopes,
                    has_osc,
                    Some(param_row_key_base(&info.id)),
                    // P1 drawer tween: supply the interpolated height only while in
                    // flight; settled rows pass None → the natural (unclipped) layout.
                    self.drawer_height_anim
                        .get(i)
                        .filter(|a| a.is_animating())
                        .map(|a| a.value()),
                );
                self.row_host.toggle_ids[i] = Some(ToggleParamIds {
                    label_id: row.label_id,
                    button_id: row.button_id,
                });
                self.toggle_cache[i] = info.spec.default > 0.5;
                self.row_host.audio_btn_ids[i] = row.audio_btn;
                self.row_host.audio_configs[i] = row.audio_config;
                self.row_host.audio_trigger_mode_badge_ids[i] = row.mode_badge_id;
                self.row_host.reindex_row(tree, i);
                cy = row.new_cy;
                continue;
            }

            let row_y = cy;
            // Per-param slider + driver/envelope/Ableton drawers — the shared
            // core. Effects nest rows under `parent` (the inner-bg panel), use
            // the default slider palette + caption-size driver-config font, and
            // gate the `E` button on `supports_envelopes`.
            let row = build_param_row(
                tree,
                Some(parent),
                x + PADDING,
                cy,
                slider_w,
                &info,
                &self.state.mod_state,
                i,
                self.param_target(),
                &SliderColors::default_slider(),
                CONFIG_BTN_FONT_SIZE,
                self.supports_envelopes,
                label_width,
                self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                !self.compact,
                Some(param_row_key_base(&info.id)),
                // P1 drawer tween: supply the interpolated height only while in
                // flight; settled rows pass None → the natural (unclipped) layout.
                self.drawer_height_anim
                    .get(i)
                    .filter(|a| a.is_animating())
                    .map(|a| a.value()),
            );
            self.row_host.slider_ids[i] = row.slider;
            self.row_host.slider_resets[i] = Some(row.slider_reset);
            self.row_host.row_catcher_ids[i] = Some(row.row_catcher);
            self.row_host.trim_ids[i] = row.trim;
            self.row_host.target_ids[i] = row.target;
            self.row_host.envelope_config_ids[i] = row.envelope_config;
            self.row_host.ableton_trim_ids[i] = row.ableton_trim;
            self.row_host.audio_trim_ids[i] = row.audio_trim;
            self.row_host.envelope_btn_ids[i] = row.envelope_btn;
            self.row_host.driver_btn_ids[i] = Some(row.driver_btn);
            self.row_host.driver_config_ids[i] = row.driver_config;
            self.row_host.ableton_config_ids[i] = row.ableton_config;
            self.row_host.audio_btn_ids[i] = Some(row.audio_btn);
            self.row_host.audio_configs[i] = row.audio_config;
            self.row_host.mod_tab_ids[i] = row.mod_tabs;
            self.sync_mod_tab_ink(tree, i);
            // Mapping-drawer chevron at the row's right edge (Author + mappable).
            // A subtle ">" that opens the sideways range/scale/offset/invert/
            // curve drawer for this binding. Sits past the D/E buttons in the
            // reserved lane; click resolves via `mapping_chevron_ids`.
            if author && info.mapping.mappable {
                let ch_x = x + PADDING + (w - PADDING * 2.0) - MAP_CHEVRON_W;
                let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                // Keyed by (row | chevron role): the chevron's identity must not
                // shift when an earlier row arms a modulator and inserts drawer
                // nodes ahead of it. See `docs/INPUT_IDENTITY_UNIFICATION.md`.
                self.row_host.mapping_chevron_ids[i] = Some(tree.add_button_keyed(
                    Some(parent),
                    ch_x,
                    ch_y,
                    MAP_CHEVRON_W,
                    DE_BUTTON_SIZE,
                    UIStyle {
                        bg_color: Color32::TRANSPARENT,
                        hover_bg_color: color::HOVER_OVERLAY,
                        pressed_bg_color: color::PRESS_OVERLAY,
                        text_color: color::CHEVRON_COLOR,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Center,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                    "\u{203A}", // ›
                    param_row_key_base(&info.id) | ROW_ROLE_CHEVRON,
                ));
                // Naming pass (UI_AUTOMATION_DESIGN.md D8/§3): one static name for
                // every row's chevron — which row comes from the selector's
                // `under_text` query, not a per-row name string.
                if let Some(id) = self.row_host.mapping_chevron_ids[i] {
                    tree.set_name(id, "inspector.param_card.mapping_chevron");
                }
            }
            self.row_host.reindex_row(tree, i);
            cy = row.new_cy;
        }
        }

        // ── "3D Shading" relight rows (docs/DEPTH_RELIGHT_DESIGN.md P5b) —
        // always drawn, greyed when the header toggle is off (no-
        // conditionally-visible-ui). ──
        self.build_relight_rows(tree, Some(parent), x + PADDING, cy, w - PADDING * 2.0);
    }

    /// The six D3 knob rows + the D4 Height From row — shared between the
    /// effect and generator card, since both hosts read/write the same
    /// `PresetInstance.relight_params` shape (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// P5b). Always drawn when the caller reaches this point (never
    /// conditioned on `self.relight.enabled` — greyed instead, per the
    /// no-conditionally-visible-ui rule), so values set while the toggle is
    /// off survive to when it's switched on.
    fn build_relight_rows(
        &mut self,
        tree: &mut UITree,
        parent: Option<NodeId>,
        x: f32,
        mut cy: f32,
        content_w: f32,
    ) {
        // Feature disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // draw no "3D Shading" label, knobs, or Height From row. The slider-id
        // slots stay `None`, so drag/reset hit-tests never match.
        if !RELIGHT_FEATURE_ENABLED {
            return;
        }
        let enabled = self.relight.enabled;
        let label_color = if enabled { color::TEXT_PRIMARY_C32 } else { color::TEXT_DIMMED_C32 };
        tree.add_label(
            parent,
            x,
            cy,
            content_w,
            ROW_HEIGHT,
            "3D Shading",
            UIStyle {
                text_color: label_color,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += ROW_HEIGHT + ROW_SPACING;

        let target = self.param_target();
        let colors = if enabled { SliderColors::default_slider() } else { relight_disabled_slider_colors() };
        let label_width = crate::slider::label_width_for_row(content_w);

        for (i, spec) in RELIGHT_FIELD_SPECS.iter().enumerate() {
            let value = self.relight.value(spec.field);
            let norm = BitmapSlider::value_to_normalized(value, spec.min, spec.max);
            let default_norm = BitmapSlider::value_to_normalized(spec.default, spec.min, spec.max);
            let value_text = format!("{value:.2}");
            let reset = PanelAction::slider_reset(
                PanelAction::Scrub(ValueRef::RelightParam(target.clone(), spec.field), ScrubPhase::Begin),
                PanelAction::Scrub(ValueRef::RelightParam(target.clone(), spec.field), ScrubPhase::Move(ScrubValue::Scalar(spec.default))),
                PanelAction::Scrub(ValueRef::RelightParam(target.clone(), spec.field), ScrubPhase::Commit),
            );
            let slider = BitmapSlider::build(
                tree,
                parent,
                Rect::new(x, cy, content_w, ROW_HEIGHT),
                Some(spec.label),
                norm,
                &value_text,
                &colors,
                FONT_SIZE,
                label_width,
                default_norm,
                reset,
                None,
            );
            self.relight_slider_ids[i] = Some(slider.ids);
            self.relight_slider_resets[i] = Some(slider.reset);
            cy += ROW_HEIGHT + ROW_SPACING;
        }

        // D4 Height From row — a 3-way segmented control.
        let opts = [
            (UiRelightHeightFrom::Auto, "Auto"),
            (UiRelightHeightFrom::Luminance, "Luminance"),
            (UiRelightHeightFrom::InvertedLuminance, "Inverted"),
        ];
        let seg_gap = 2.0;
        let seg_w = (content_w - seg_gap * (opts.len() - 1) as f32) / opts.len() as f32;
        for (i, (opt, text)) in opts.into_iter().enumerate() {
            let active = self.relight.height_from == opt;
            let bg = if enabled && active { color::SLIDER_FILL_C32 } else { color::BG_3 };
            let btn_id = tree.add_button(
                parent,
                x + (seg_w + seg_gap) * i as f32,
                cy,
                seg_w,
                ROW_HEIGHT,
                UIStyle {
                    bg_color: bg,
                    text_color: if enabled { color::TEXT_WHITE_C32 } else { color::TEXT_DIMMED_C32 },
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Center,
                    corner_radius: color::SMALL_RADIUS,
                    ..UIStyle::default()
                },
                text,
            );
            self.relight_height_btn_ids[i] = Some(btn_id);
        }
    }

    fn build_generator(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.toggle_cache.iter_mut().for_each(|v| *v = false);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        // ── Card frame + header (host) ──
        let border_color = self.base_border_color();
        let view = self.generator_card_view(border_color);
        let h = self.compute_height() - CARD_BOTTOM_MARGIN;
        // D17 "spawn pop" — see the matching comment in `build_effect`. Not
        // currently fired for the generator card (no `reconcile_cards`-style
        // reuse/new detection wired for it — see `spawn_scale`'s call site),
        // but the geometry mechanics are shared so it's a one-line wire-up
        // later if that seam gets added.
        let frame_rect = scaled_card_rect(rect.x, rect.y, rect.width, h, self.spawn_scale.value());
        self.host.build(tree, &view, frame_rect);
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        self.name_label_id = self.host.node_id_for_key(KEY_NAME);
        self.change_btn_id = self.host.node_id_for_key(KEY_CHANGE);
        self.relight_btn_id = self.host.node_id_for_key(KEY_RELIGHT);
        self.chevron_btn_id = self.host.node_id_for_key(KEY_CHEVRON);
        self.cog_btn_id = self.host.node_id_for_key(KEY_COG);
        if let Some(cog) = self.cog_btn_id {
            self.add_cog_dots(tree, cog);
        }

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;

        // ── Params (if not collapsed) — the relight rows below always draw
        // when expanded, regardless of whether this card has any regular
        // params (`docs/DEPTH_RELIGHT_DESIGN.md` P5b: "3D Shading" is a
        // per-instance flag independent of the graph's own param list). ──
        if !self.is_collapsed {
            let content_w = inner_w - PADDING * 2.0;
            let cx = inner_x + PADDING;
            let mut cy = inner_y + HEADER_HEIGHT + HEADER_BODY_GAP;
            // Same `row_geometry` helper the effect card uses (D2), so
            // generator slider rows can't drift from the effect card's lane
            // math. `author` gates both the chevron lane reservation and the
            // glyph draw + row-id scheme below.
            let author = self.context == CardContext::Author;
            let RowGeometry { label_width, slider_w } = row_geometry(content_w, author);

            if !self.rows.is_empty() {
            self.row_host.section_header_ids.clear();
            let runs = self.section_runs();
            for (start, len, section) in runs {
                if let Some(name) = &section {
                    let folded = self.section_folded.get(name).copied().unwrap_or(false);
                    let header_id = self.build_section_header(
                        tree,
                        None,
                        cx,
                        cy,
                        content_w,
                        name,
                        folded,
                        len,
                        param_row_key_base(&self.rows[start].id),
                    );
                    self.row_host.section_header_ids.push((header_id, name.clone()));
                self.row_host.row_index.insert(tree.widget_of(header_id), start, RowRole::SectionHeader);
                    cy += ROW_HEIGHT + ROW_SPACING;
                    if folded {
                        // See the effect-card twin of this branch for why
                        // this clears rather than leaves stale ids.
                        for i in start..start + len {
                            self.clear_row_ids(i);
                        }
                        continue;
                    }
                }

            for i in start..start + len {
                let info = self.rows[i].clone();

                if info.spec.is_toggle || info.spec.is_trigger {
                    // Toggle / Trigger row — shared builder (Task A of §8.4
                    // P3b unified this with the effect card's toggle/trigger
                    // rendering; see `build_toggle_trigger_row`'s doc comment).
                    // ON/OFF for sticky toggles, ▶ for momentary fire-once
                    // triggers; `is_trigger` additionally reaches the audio-mod
                    // "A" button + drawer (D5b). Click handler dispatches
                    // differently (toggle vs fire) based on the is_trigger flag.
                    let has_osc = self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some();
                    let row = build_toggle_trigger_row(
                        tree,
                        None,
                        cx,
                        cy,
                        slider_w,
                        &info,
                        &self.state.mod_state,
                        i,
                        self.param_target(),
                        FONT_SIZE,
                        true, // generators always reserve the driver-column gap
                        has_osc,
                        Some(param_row_key_base(&info.id)),
                        // P1 drawer tween: supply the interpolated height only while in
                        // flight; settled rows pass None → the natural (unclipped) layout.
                        self.drawer_height_anim
                            .get(i)
                            .filter(|a| a.is_animating())
                            .map(|a| a.value()),
                    );
                    self.row_host.toggle_ids[i] = Some(ToggleParamIds {
                        label_id: row.label_id,
                        button_id: row.button_id,
                    });
                    self.toggle_cache[i] = info.spec.default > 0.5;
                    self.row_host.audio_btn_ids[i] = row.audio_btn;
                    self.row_host.audio_configs[i] = row.audio_config;
                    self.row_host.audio_trigger_mode_badge_ids[i] = row.mode_badge_id;
                    self.row_host.reindex_row(tree, i);
                    cy = row.new_cy;
                } else {
                    // Slider row — shared per-param core. Generators parent rows
                    // flat to the root (`None`), use the gen-param slider palette,
                    // the body-size driver-config font, and always show the `E`
                    // button (generators always support envelopes).
                    let row_y = cy;
                    let row = build_param_row(
                        tree,
                        None,
                        cx,
                        cy,
                        slider_w,
                        &info,
                        &self.state.mod_state,
                        i,
                        self.param_target(),
                        &SliderColors::default_slider(),
                        FONT_SIZE,
                        true,
                        label_width,
                        self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                        !self.compact,
                        Some(param_row_key_base(&info.id)),
                        // P1 drawer tween: interpolated height while in flight only.
                        self.drawer_height_anim
                            .get(i)
                            .filter(|a| a.is_animating())
                            .map(|a| a.value()),
                    );
                    self.row_host.slider_ids[i] = row.slider;
                    self.row_host.slider_resets[i] = Some(row.slider_reset);
                    self.row_host.row_catcher_ids[i] = Some(row.row_catcher);
                    self.row_host.trim_ids[i] = row.trim;
                    self.row_host.target_ids[i] = row.target;
                    self.row_host.envelope_config_ids[i] = row.envelope_config;
                    self.row_host.ableton_trim_ids[i] = row.ableton_trim;
                    self.row_host.audio_trim_ids[i] = row.audio_trim;
                    self.row_host.envelope_btn_ids[i] = row.envelope_btn;
                    self.row_host.driver_btn_ids[i] = Some(row.driver_btn);
                    self.row_host.driver_config_ids[i] = row.driver_config;
                    self.row_host.ableton_config_ids[i] = row.ableton_config;
                    self.row_host.audio_btn_ids[i] = Some(row.audio_btn);
                    self.row_host.audio_configs[i] = row.audio_config;
                    self.row_host.mod_tab_ids[i] = row.mod_tabs;
                    self.sync_mod_tab_ink(tree, i);
                    // Mapping-drawer chevron at the row's right edge (Author +
                    // mappable) — identical to the effect card. Opens the same
                    // sideways range/scale/offset/invert/curve drawer; click
                    // resolves via the shared `mapping_chevron_ids`.
                    if author && info.mapping.mappable {
                        let ch_x = cx + content_w - MAP_CHEVRON_W;
                        let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                        self.row_host.mapping_chevron_ids[i] = Some(tree.add_button_keyed(
                            None,
                            ch_x,
                            ch_y,
                            MAP_CHEVRON_W,
                            DE_BUTTON_SIZE,
                            UIStyle {
                                bg_color: Color32::TRANSPARENT,
                                hover_bg_color: color::HOVER_OVERLAY,
                                pressed_bg_color: color::PRESS_OVERLAY,
                                text_color: color::CHEVRON_COLOR,
                                font_size: FONT_SIZE,
                                text_align: TextAlign::Center,
                                corner_radius: color::SMALL_RADIUS,
                                ..UIStyle::default()
                            },
                            "\u{203A}", // ›
                            param_row_key_base(&info.id) | ROW_ROLE_CHEVRON,
                        ));
                    }
                    self.row_host.reindex_row(tree, i);
                    cy = row.new_cy;
                }
            }
            }
            } // end if !self.rows.is_empty()

            // ── String param rows (clickable text fields) ──
            for (si, sp) in self.string_param_info.iter().enumerate() {
                let display = if sp.value.is_empty() {
                    format!("{}: (empty)", sp.name)
                } else {
                    format!("{}: {}", sp.name, sp.value)
                };
                self.string_param_btn_ids[si] = Some(tree.add_button(
                    None,
                    cx,
                    cy,
                    content_w,
                    ROW_HEIGHT,
                    UIStyle {
                        bg_color: color::INSPECTOR_BG,
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                    &display,
                ));
                cy += ROW_HEIGHT + ROW_SPACING;
            }

            // ── "3D Shading" relight rows (docs/DEPTH_RELIGHT_DESIGN.md
            // P5b) — always drawn when expanded, greyed when the header
            // toggle is off (no-conditionally-visible-ui). ──
            self.build_relight_rows(tree, None, cx, cy, content_w);
        } // end if !self.is_collapsed

        self.node_count = tree.count() - self.first_node;
    }

    /// Per-frame value sync from the manifest's id-keyed slot channel
    /// (`ui_translate::with_param_slots`). JOINS each slot onto the built row
    /// carrying the same id (BUG-313) — never by position, so no second filter
    /// can drift out of alignment with the structural build. A manifest id with
    /// no row (a `card_visible: false` param the curated card skipped) simply
    /// misses the join and is ignored.
    pub fn sync_values(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        match self.kind {
            ParamCardKind::Effect => self.sync_values_effect(tree, slots),
            ParamCardKind::Generator => self.sync_values_generator(tree, slots),
        }
    }

    /// Test-only positional adapter: pair each slot with its row's id in order,
    /// then feed the real id-keyed [`sync_values`](Self::sync_values). Unit
    /// tests build a known config where row order == the manifest channel
    /// order, so this preserves their intent; the id-join itself is proven by
    /// the projection test (a hidden param interleaved among visible ones).
    #[cfg(test)]
    pub(crate) fn sync_values_positional(
        &mut self,
        tree: &mut UITree,
        values: &[crate::view::UiParamSlot],
    ) {
        let pairs: Vec<(String, crate::view::UiParamSlot)> = self
            .rows
            .iter()
            .zip(values.iter().copied())
            .map(|(r, v)| (r.id.to_string(), v))
            .collect();
        let mut it = pairs.iter().map(|(id, v)| (id.as_str(), *v));
        self.sync_values(tree, &mut it);
    }

    /// The shared id-join value push (both card kinds). Iterates the manifest
    /// slot channel, resolves each id to its row via `row_id_index`, and pushes
    /// the value; tracks coverage so a built row that lost its manifest entry
    /// (INV-6) is caught loudly instead of freezing silently. No positional
    /// coupling anywhere — the join key is the id.
    fn sync_row_values_by_id(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        self.row_value_synced.clear();
        self.row_value_synced.resize(self.rows.len(), false);
        for (id, slot) in slots {
            let Some(&i) = self.row_id_index.get(id) else {
                continue;
            };
            if let Some(b) = self.base_values.get_mut(i) {
                *b = slot.base;
            }
            self.sync_param_value(tree, i, slot.value);
            if let Some(c) = self.row_value_synced.get_mut(i) {
                *c = true;
            }
        }
        for (i, synced) in self.row_value_synced.iter().enumerate() {
            if !*synced {
                debug_assert!(
                    false,
                    "BUG-313/INV-6: built card row {} (id {:?}) has no live manifest entry",
                    i, self.rows[i].id,
                );
                crate::panels::param_slider_shared::warn_join_gap_once(self.rows[i].id.as_ref());
            }
        }
    }

    /// Re-pack the header badges + resize the name cell after the active-badge
    /// set changes in the sync path (no rebuild). Mirrors the packed layout
    /// `build_effect_header` computes, so a toggled badge lands flush-right and
    /// the name reclaims the freed width without a card rebuild.
    fn reposition_effect_badges(&self, tree: &mut UITree) {
        let (Some(toggle_btn_id), Some(name_clip_id), Some(mod_badge_bg_id)) =
            (self.toggle_btn_id, self.name_clip_id, self.mod_badge_bg_id)
        else {
            return;
        };
        let toggle_x = tree.get_bounds(toggle_btn_id).x;
        let badge_y = tree.get_bounds(mod_badge_bg_id).y;
        // The name cell's left edge is the region's left bound for centering.
        let content_left = tree.get_bounds(name_clip_id).x;
        let badges = effect_badge_layout(
            content_left,
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
            self.state.has_audio,
        );
        let park = toggle_x - GAP - BADGE_W;
        for (bg, txt, x) in [
            (self.mod_badge_bg_id, self.mod_badge_text_id, badges.mod_x),
            (self.abl_badge_bg_id, self.abl_badge_text_id, badges.abl_x),
            (self.env_badge_bg_id, self.env_badge_text_id, badges.env_x),
            (self.drv_badge_bg_id, self.drv_badge_text_id, badges.drv_x),
            (self.aud_badge_bg_id, self.aud_badge_text_id, badges.aud_x),
        ] {
            let r = Rect::new(x.unwrap_or(park), badge_y, BADGE_W, BADGE_H);
            if let Some(bg) = bg {
                tree.set_bounds(bg, r);
            }
            if let Some(txt) = txt {
                tree.set_bounds(txt, r);
            }
        }
        let name_b = tree.get_bounds(name_clip_id);
        let name_w = (badges.name_right - name_b.x).max(10.0);
        tree.set_bounds(
            name_clip_id,
            Rect::new(name_b.x, name_b.y, name_w, name_b.height),
        );
    }

    fn sync_values_effect(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        // Shared lookup (checks both slider AND toggle/trigger row labels —
        // effect cards can copy-flash either kind now, same as generator's).
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        // Toggle state dirty-check
        if self.enabled != self.cached_enabled {
            self.cached_enabled = self.enabled;
            if let Some(toggle_btn_id) = self.toggle_btn_id {
                tree.set_style(toggle_btn_id, toggle_btn_style(self.enabled));
                tree.set_text(toggle_btn_id, if self.enabled { "ON" } else { "OFF" });
            }
        }

        // Badge visibility dirty-check
        if self.state.has_env != self.cached_has_env
            || self.state.has_drv != self.cached_has_drv
            || self.state.has_abl != self.cached_has_abl
            || self.state.has_audio != self.cached_has_audio
            || self.state.has_graph_mod != self.cached_has_graph_mod
        {
            self.cached_has_env = self.state.has_env;
            self.cached_has_drv = self.state.has_drv;
            self.cached_has_abl = self.state.has_abl;
            self.cached_has_audio = self.state.has_audio;
            self.cached_has_graph_mod = self.state.has_graph_mod;
            for (id, visible) in [
                (self.abl_badge_bg_id, self.cached_has_abl),
                (self.abl_badge_text_id, self.cached_has_abl),
                (self.env_badge_bg_id, self.cached_has_env),
                (self.env_badge_text_id, self.cached_has_env),
                (self.drv_badge_bg_id, self.cached_has_drv),
                (self.drv_badge_text_id, self.cached_has_drv),
                (self.aud_badge_bg_id, self.cached_has_audio),
                (self.aud_badge_text_id, self.cached_has_audio),
                (self.mod_badge_bg_id, self.cached_has_graph_mod),
                (self.mod_badge_text_id, self.cached_has_graph_mod),
            ] {
                if let Some(id) = id {
                    tree.set_visible(id, visible);
                }
            }
            // Re-pack the badges + resize the name cell now the active set changed.
            // The header keeps the one accent — a graph override lights the MOD
            // badge only, it never recolours the header.
            self.reposition_effect_badges(tree);
        }

        // Skip slider sync if collapsed (no rows built → nothing to join, and
        // the coverage check would misfire on the un-synced rows).
        if self.is_collapsed {
            return;
        }

        // Per-param slider/toggle/trigger values + label — id-joined, shared
        // with `sync_values_generator`.
        self.sync_row_values_by_id(tree, slots);
    }

    /// Per-parameter value/label sync shared by both card kinds. Slider rows
    /// redraw their fill + value text on change; a toggle row flips its
    /// ON/OFF button; a trigger row does nothing (the fire counter isn't
    /// user-visible). Kept as one function so the two kinds can't drift back
    /// apart the way `build_effect_sliders` and `build_generator`'s toggle
    /// rendering did (§8.4 P3b Task A).
    fn sync_param_value(&mut self, tree: &mut UITree, i: usize, val: f32) {
        let info = &self.rows[i];

        // Label dirty-check (slider rows only — toggle/trigger rows have
        // their label baked into the row at build time).
        if !info.spec.is_toggle && !info.spec.is_trigger {
            let new_label = Some(info.spec.name.clone());
            if self.label_cache[i] != new_label {
                self.label_cache[i] = new_label;
                if let Some(ref ids) = self.row_host.slider_ids[i]
                    && let Some(label) = ids.label
                {
                    tree.set_text(label, &info.spec.name);
                }
            }
        }

        if info.spec.is_toggle {
            let on = val > 0.5;
            if on != self.toggle_cache[i] {
                self.toggle_cache[i] = on;
                if let Some(ref ids) = self.row_host.toggle_ids[i] {
                    tree.set_style(ids.button_id, toggle_btn_style(on));
                    tree.set_text(ids.button_id, if on { "ON" } else { "OFF" });
                }
            }
        } else if info.spec.is_trigger {
            // Trigger button stays neutral — the counter value isn't
            // user-visible; nothing to re-render per frame.
        } else if val != self.param_cache[i] || self.param_cache[i].is_nan() {
            // P2 value-change flash: only for a genuine change (not the
            // post-configure NaN resync) and only while this card's slider
            // isn't being dragged (the drag is its own feedback).
            if !self.param_cache[i].is_nan()
                && !self.drag.is_dragging()
                && let Some(flash) = self.value_flash.get_mut(i)
            {
                flash.fire(color::MOTION_SLOW_MS);
            }
            self.param_cache[i] = val;
            // P2 value snap-back (D15): a reset just retargeted this row's
            // `value_snapback` (`begin_value_snapback`, same frame, before this
            // poll) — draw the FILL at its just-`snap()`ped starting point
            // instead of jumping straight to the value's normalized position;
            // `tick_value_flash` eases it forward every frame after. Any other
            // value change (drag commit, automation, undo) has no animating
            // snapback here, so the override is `None` and the fill draws the
            // value exactly as before. The normalize→format→update math itself
            // is the shared §5.6 push both cards use (`RowHost::push_slider_value`).
            let display_norm_override = self
                .value_snapback
                .get(i)
                .filter(|a| a.is_animating())
                .map(|a| a.value());
            self.row_host
                .push_slider_value(tree, i, val, &info.spec, display_norm_override);
        }
    }

    fn sync_values_generator(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        self.sync_row_values_by_id(tree, slots);
    }

    /// Find the original param name for a label node ID (slider or toggle).
    fn find_label_name(&self, label_id: NodeId) -> String {
        for (pi, s) in self.row_host.slider_ids.iter().enumerate() {
            if let Some(ids) = s
                && ids.label == Some(label_id)
            {
                return self
                    .rows
                    .get(pi)
                    .map(|p| p.spec.name.clone())
                    .unwrap_or_default();
            }
        }
        for (pi, t) in self.row_host.toggle_ids.iter().enumerate() {
            if let Some(ids) = t
                && ids.label_id == Some(label_id)
            {
                return self
                    .rows
                    .get(pi)
                    .map(|p| p.spec.name.clone())
                    .unwrap_or_default();
            }
        }
        String::new()
    }

    pub fn sync_effect_name(&mut self, tree: &mut UITree, name: &str) {
        self.name = name.into();
        if let Some(name_label_id) = self.name_label_id {
            tree.set_text(name_label_id, name);
        }
    }

    pub fn sync_enabled(&mut self, _tree: &mut UITree, enabled: bool) {
        // Just update the field — tree update happens in sync_values() dirty-check.
        self.enabled = enabled;
    }

    pub fn sync_gen_type_name(&mut self, tree: &mut UITree, name: &str) {
        self.name = name.into();
        if let Some(name_label_id) = self.name_label_id {
            tree.set_text(name_label_id, name);
        }
    }

    /// Update a string param value and its display text (generator kind).
    pub fn sync_string_param(&mut self, tree: &mut UITree, index: usize, value: &str) {
        if let Some(sp) = self.string_param_info.get_mut(index) {
            sp.value = value.to_string();
            if let Some(Some(btn_id)) = self.string_param_btn_ids.get(index).copied() {
                let display = if value.is_empty() {
                    format!("{}: (empty)", sp.name)
                } else {
                    format!("{}: {}", sp.name, value)
                };
                tree.set_text(btn_id, &display);
            }
        }
    }
}
