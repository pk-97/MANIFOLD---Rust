//! Input routing (click/pointer-down/drag/drag-end + intent registration)
//! for [`ParamCardPanel`] (P-S4 split of `param_card.rs`).

use super::*;

impl ParamCardPanel {
    pub fn handle_click(&mut self, node_id: NodeId, tree: &UITree) -> Vec<PanelAction> {
        let id = node_id;

        // "3D Shading" header toggle + D4 Height From row — card-level chrome,
        // not row-indexed: relight has no `self.rows` slot to key against
        // (RowRole::RelightToggle/RelightHeightBtn/RelightSlider stay outside
        // `row_index`; see the P2 landing report).
        if self.relight_btn_id == Some(id) {
            return vec![PanelAction::Params(ParamsAction::RelightToggle(self.param_target()))];
        }
        for (i, btn) in self.relight_height_btn_ids.iter().enumerate() {
            if *btn == Some(id) {
                let opt = [
                    UiRelightHeightFrom::Auto,
                    UiRelightHeightFrom::Luminance,
                    UiRelightHeightFrom::InvertedLuminance,
                ][i];
                return vec![PanelAction::Params(ParamsAction::RelightHeightFromChanged(self.param_target(), opt))];
            }
        }

        // Card-level chrome — per-card, not per-row, and kind-specific
        // (D5's "not part of the disease": these are folded here, before the
        // row lookup, rather than migrated into `RowIndex`).
        match self.kind {
            ParamCardKind::Effect => {
                let ei = self.effect_index;
                if self.toggle_btn_id == Some(id) {
                    return vec![PanelAction::Params(ParamsAction::EffectToggle(ei))];
                }
                if self.chevron_btn_id == Some(id) {
                    return vec![PanelAction::Params(ParamsAction::EffectCollapseToggle(ei))];
                }
                if self.cog_btn_id == Some(id) {
                    return vec![PanelAction::Root(RootAction::OpenGraphEditor(ei))];
                }
                if self.border_id == Some(id)
                    || self.header_bg_id == Some(id)
                    || self.inner_bg_id == Some(id)
                    || self.drag_icon_id == Some(id)
                    || self.name_label_id == Some(id)
                {
                    return vec![PanelAction::Params(ParamsAction::EffectCardClicked(ei))];
                }
            }
            ParamCardKind::Generator => {
                if self.chevron_btn_id == Some(id) {
                    return vec![PanelAction::Params(ParamsAction::GenCollapseToggle)];
                }
                if self.change_btn_id == Some(id) {
                    return vec![PanelAction::Params(ParamsAction::GenTypeClicked(self.layer_id.clone()))];
                }
                if self.cog_btn_id == Some(id) {
                    return vec![PanelAction::Root(RootAction::OpenGeneratorGraphEditor)];
                }
                if self.header_bg_id == Some(id)
                    || self.name_label_id == Some(id)
                    || self.border_id == Some(id)
                {
                    return vec![PanelAction::Params(ParamsAction::GenCardClicked)];
                }
                // String param rows carry no `RowRole` (`self.rows` has no
                // slot for them — `ParamCardStringInfo` is a separate,
                // generator-only array); out of `row_index` scope, kept here.
                for (si, &btn_id) in self.string_param_btn_ids.iter().enumerate() {
                    if btn_id == Some(id) {
                        if self.string_param_info.get(si).is_some_and(|sp| sp.use_dropdown) {
                            return vec![PanelAction::Params(ParamsAction::GenStringParamDropdownClicked(si))];
                        }
                        return vec![PanelAction::Params(ParamsAction::GenStringParamClicked(si))];
                    }
                }
            }
        }

        // Every remaining row-shaped click resolves through the index built
        // as this row's controls were minted (D5, `docs/WIDGET_TREE_DESIGN.md`
        // P2) — the ONLY sanctioned way this function identifies a row
        // element.
        let widget = tree.widget_of(id);
        if let Some((row, role)) = self.row_host.row_index.get(widget) {
            let target = self.param_target();
            return self.row_host.row_action(
                target,
                row,
                role,
                id,
                &self.rows,
                &self.base_values,
                &self.osc_addresses,
                &mut self.state.mod_state,
                &mut self.mod_active_tab,
                &mut self.copied_flash,
                &mut self.section_folded,
            );
        }

        Vec::new()
    }

    /// The trim-handle node ids for a modulator kind. The three kinds keep
    /// separate id vectors (they overlay the same track simultaneously), and
    /// this is the one place a `TrimKind` selects between them.
    fn trim_ids_for(&self, kind: TrimKind) -> &[Option<TrimHandleIds>] {
        match kind {
            TrimKind::Driver => &self.row_host.trim_ids,
            TrimKind::Ableton => &self.row_host.ableton_trim_ids,
            TrimKind::Audio => &self.row_host.audio_trim_ids,
        }
    }

    /// The current `[min, max]` output sub-range a kind's trim handles
    /// represent at param index `pi`. Driver/audio read card `mod_state` and
    /// default to the full range; Ableton reads the mapping range on
    /// `rows` and returns `None` when the param has no mapping — the old
    /// `ableton_range` guard, preserved so an Ableton drag can't proceed
    /// without a mapping to edit.
    fn trim_range(&self, kind: TrimKind, pi: usize) -> Option<(f32, f32)> {
        match kind {
            TrimKind::Driver => Some((
                self.state.mod_state.trim_min.get(pi).copied().unwrap_or(0.0),
                self.state.mod_state.trim_max.get(pi).copied().unwrap_or(1.0),
            )),
            TrimKind::Ableton => self.rows[pi].mapping.ableton_range,
            TrimKind::Audio => Some((
                self.state
                    .mod_state
                    .audio_range_min
                    .get(pi)
                    .copied()
                    .unwrap_or(0.0),
                self.state
                    .mod_state
                    .audio_range_max
                    .get(pi)
                    .copied()
                    .unwrap_or(1.0),
            )),
        }
    }

    /// Write a kind's live trim range at param index `pi` back to its card-side
    /// store during a drag. The mirror of [`trim_range`].
    fn set_trim_range(&mut self, kind: TrimKind, pi: usize, min: f32, max: f32) {
        match kind {
            TrimKind::Driver => {
                if let Some(v) = self.state.mod_state.trim_min.get_mut(pi) {
                    *v = min;
                }
                if let Some(v) = self.state.mod_state.trim_max.get_mut(pi) {
                    *v = max;
                }
            }
            TrimKind::Ableton => {
                self.rows[pi].mapping.ableton_range = Some((min, max));
            }
            TrimKind::Audio => {
                if let Some(v) = self.state.mod_state.audio_range_min.get_mut(pi) {
                    *v = min;
                }
                if let Some(v) = self.state.mod_state.audio_range_max.get_mut(pi) {
                    *v = max;
                }
            }
        }
    }

    /// Unified pointer-down hit-testing for both card kinds. Steps 1-4 grab the
    /// modulation widgets (the envelope target handle, the envelope decay slider,
    /// driver/Ableton/audio trim bars); step 5 is the param slider, with the
    /// proximity catch-zones for the target handle and driver trim handles. The
    /// emitted target comes from `param_target()`, so effect and generator share
    /// one path; toggle/trigger rows (generator-only, no slider widget) are
    /// skipped in step 5.
    /// `tree` is read-only and mandatory (BUG-259): all geometry comes from
    /// live bounds, never the build-time cache — in-place scroll shifts node
    /// y without refreshing panel caches (BUG-257).
    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2, tree: &UITree) -> Vec<PanelAction> {
        let target = self.param_target();

        // Resolve which row (if any) owns the pressed node through the same
        // index `handle_click` uses (D5) — replaces the old per-collection
        // SCANS across every row with one lookup, then direct field reads on
        // the resolved row. Relight knobs (below) aren't row-indexed
        // (relight has no `self.rows` slot); a lookup miss falls through to
        // them, matching the old gauntlet's tail.
        if let Some((row, role)) = self.row_host.row_index.get(tree.widget_of(node_id)) {
            match role {
                RowRole::EnvelopeConfig => {
                    if let Some(c) = &self.row_host.envelope_config_ids[row]
                        && node_id == c.decay_slider.track
                    {
                        self.drag.begin(ParamDragTarget::EnvDecay { index: row }, pos);
                        let norm = BitmapSlider::x_to_normalized(
                            TrackSpan::of(tree.get_bounds(c.decay_slider.track)),
                            pos.x,
                        );
                        let decay = norm.clamp(0.0, 1.0) * ENV_DECAY_MAX;
                        let pid = self.rows[row].id.clone();
                        return vec![
                            PanelAction::Scrub(ValueRef::EnvDecay(target.clone(), pid.clone()), ScrubPhase::Begin),
                            PanelAction::Scrub(ValueRef::EnvDecay(target, pid), ScrubPhase::Move(ScrubValue::Scalar(decay))),
                        ];
                    }
                    Vec::new()
                }
                RowRole::AudioConfig => {
                    if let Some((dids, _)) = &self.row_host.audio_configs[row] {
                        for (si, which) in [
                            (0usize, AudioShapeParam::Sensitivity),
                            (1, AudioShapeParam::Attack),
                            (2, AudioShapeParam::Release),
                        ] {
                            if let Some(sl) = dids.sliders.get(si)
                                && node_id == sl.track
                            {
                                let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(sl.track)), pos.x)
                                    .clamp(0.0, 1.0);
                                let value = audio_shape_value_from_norm(which, norm);
                                self.drag.begin(ParamDragTarget::AudioShape { index: row, param: which }, pos);
                                let pid = self.rows[row].id.clone();
                                return vec![
                                    PanelAction::Scrub(ValueRef::AudioModShape(target.clone(), pid.clone(), which), ScrubPhase::Begin),
                                    PanelAction::Scrub(ValueRef::AudioModShape(target, pid, which), ScrubPhase::Move(ScrubValue::Scalar(value))),
                                ];
                            }
                        }
                        // Step-Amount slider (only present while Action=Step,
                        // D8) — `DrawerIds.sliders[3]`, one past the three
                        // shaping sliders above.
                        if let Some(sl) = dids.sliders.get(3)
                            && node_id == sl.track
                        {
                            let info = &self.rows[row];
                            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(sl.track)), pos.x)
                                .clamp(0.0, 1.0);
                            let mut value = norm_to_step_amount(norm, info.spec.min, info.spec.max);
                            if info.spec.whole_numbers {
                                value = value.round();
                            }
                            self.drag.begin(ParamDragTarget::StepAmount { index: row }, pos);
                            let pid = self.rows[row].id.clone();
                            return vec![
                                PanelAction::Scrub(ValueRef::AudioModStepAmount(target.clone(), pid.clone()), ScrubPhase::Begin),
                                PanelAction::Scrub(ValueRef::AudioModStepAmount(target, pid), ScrubPhase::Move(ScrubValue::Scalar(value))),
                            ];
                        }
                    }
                    Vec::new()
                }
                RowRole::Slider => {
                    // Envelope target handle (exact hit).
                    if let Some(t) = &self.row_host.target_ids[row]
                        && node_id == t.target_bar_id
                    {
                        self.drag.begin(ParamDragTarget::EnvTarget { index: row }, pos);
                        return vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                    }
                    // Trim bars (exact hit) — driver, Ableton, audio, same
                    // probe order the old three-loop scan used.
                    for kind in [TrimKind::Driver, TrimKind::Ableton, TrimKind::Audio] {
                        if let Some(t) = self.trim_ids_for(kind)[row].as_ref() {
                            if node_id == t.min_bar_id {
                                self.drag.begin(ParamDragTarget::Trim { kind, index: row, is_min: true }, pos);
                                return vec![PanelAction::Scrub(ValueRef::Trim(kind, target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                            }
                            if node_id == t.max_bar_id {
                                self.drag.begin(ParamDragTarget::Trim { kind, index: row, is_min: false }, pos);
                                return vec![PanelAction::Scrub(ValueRef::Trim(kind, target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                            }
                        }
                    }
                    // Toggle/trigger rows have no slider widget to drag.
                    if self.rows.get(row).map(|i| i.spec.is_toggle || i.spec.is_trigger).unwrap_or(false) {
                        return Vec::new();
                    }
                    let Some(ids) = self.row_host.slider_ids[row] else {
                        return Vec::new();
                    };
                    // Only the track itself and a trim FILL overlay (visual-
                    // only, no exact-hit check of its own) reach the param
                    // drag below — min/max bars and the target handle were
                    // already handled above (matches the old gauntlet's
                    // "track or fill/target overlay" reachability).
                    let is_overlay_fill = self.row_host.trim_ids[row].as_ref().is_some_and(|t| node_id == t.fill_id)
                        || self.row_host.ableton_trim_ids[row].as_ref().is_some_and(|t| node_id == t.fill_id)
                        || self.row_host.audio_trim_ids[row].as_ref().is_some_and(|t| node_id == t.fill_id);
                    if node_id != ids.track && !is_overlay_fill {
                        return Vec::new();
                    }
                    // If driver is expanded, check proximity to trim handles
                    // before falling through to param drag.
                    if self.state.mod_state.driver_expanded.get(row).copied().unwrap_or(false)
                        && self.row_host.trim_ids[row].is_some()
                    {
                        let tmin = self.state.mod_state.trim_min.get(row).copied().unwrap_or(0.0);
                        let tmax = self.state.mod_state.trim_max.get(row).copied().unwrap_or(1.0);
                        // Live bounds + the shared geometry fn: the zone can never
                        // drift from the drawn bars (BUG-258) or a scroll (BUG-259).
                        let bars = trim_bar_rects(tree.get_bounds(ids.track), tmin, tmax);
                        let min_center = bars.min_bar.x + TRIM_BAR_W * 0.5;
                        let max_center = bars.max_bar.x + TRIM_BAR_W * 0.5;
                        let hit_zone = 8.0; // px proximity zone for trim handles
                        let dist_min = (pos.x - min_center).abs();
                        let dist_max = (pos.x - max_center).abs();
                        if dist_min < hit_zone && dist_min <= dist_max {
                            self.drag.begin(
                                ParamDragTarget::Trim { kind: TrimKind::Driver, index: row, is_min: true },
                                pos,
                            );
                            return vec![PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                        }
                        if dist_max < hit_zone {
                            self.drag.begin(
                                ParamDragTarget::Trim { kind: TrimKind::Driver, index: row, is_min: false },
                                pos,
                            );
                            return vec![PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                        }
                    }
                    // If the envelope is armed, the orange target handle gets
                    // an ~8px proximity catch-zone so it's grabbable by feel.
                    if self.state.mod_state.envelope_expanded.get(row).copied().unwrap_or(false) {
                        let tgt = self.state.mod_state.target_norm.get(row).copied().unwrap_or(1.0);
                        let bar = target_bar_rect(tree.get_bounds(ids.track), tgt);
                        let target_center = bar.x + TARGET_BAR_W * 0.5;
                        if (pos.x - target_center).abs() < 8.0 {
                            self.drag.begin(ParamDragTarget::EnvTarget { index: row }, pos);
                            return vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(target, self.rows[row].id.clone()), ScrubPhase::Begin)];
                        }
                    }
                    // No trim/target handle nearby — normal param slider drag.
                    self.drag.begin(ParamDragTarget::Param { index: row }, pos);
                    let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
                    let info = &self.rows[row];
                    let val = BitmapSlider::normalized_to_value(norm, info.spec.min, info.spec.max);
                    let val = if info.spec.whole_numbers { val.round() } else { val };
                    vec![
                        PanelAction::Scrub(
                            ValueRef::Param(target.clone(), self.rows[row].id.clone()),
                            ScrubPhase::Begin,
                        ),
                        PanelAction::Scrub(
                            ValueRef::Param(target, self.rows[row].id.clone()),
                            ScrubPhase::Move(ScrubValue::Scalar(val)),
                        ),
                    ]
                }
                _ => Vec::new(),
            }
        } else {
            // D3 relight-knob tracks (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) —
            // NOT row-indexed (no `self.rows` slot for a fixed 6-knob array);
            // same shape as the param-slider hit-test above, minus the
            // trim/target overlay checks (relight rows carry no modulation).
            // Always live even while the toggle is off (rows render greyed,
            // not hidden, and edits while off must still take effect).
            for (slider, spec) in self.relight_slider_ids.iter().zip(RELIGHT_FIELD_SPECS.iter()) {
                if let Some(ids) = slider
                    && node_id == ids.track
                {
                    let field = spec.field;
                    self.drag.begin(ParamDragTarget::Relight { field }, pos);
                    let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
                    let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
                    return vec![
                        PanelAction::Scrub(ValueRef::RelightParam(target.clone(), field), ScrubPhase::Begin),
                        PanelAction::Scrub(ValueRef::RelightParam(target, field), ScrubPhase::Move(ScrubValue::Scalar(val))),
                    ];
                }
            }
            Vec::new()
        }
    }

    /// Drag-move dispatch. The state mutation + tree repositioning is identical
    /// for both kinds; only the emitted [`PanelAction`] variant differs, so the
    /// body is shared and branches on `kind` at each emission point.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        // Envelope target handle drag — update depth, reposition the orange bar
        // along the parameter's own track, dispatch the Target change.
        if let Some(pi) = self.drag.env_target_index()
            && let Some(slider) = self.row_host.slider_ids.get(pi).and_then(|s| s.as_ref())
        {
            // Live bounds, not the cached `track_rect`: in-place scroll shifts
            // the tree nodes without refreshing the cache, so its y is stale.
            let track_rect = tree.get_bounds(slider.track);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos.x);
            if let Some(v) = self.state.mod_state.target_norm.get_mut(pi) {
                *v = norm;
            }
            if let Some(t) = self.row_host.target_ids.get(pi).and_then(|t| t.as_ref()) {
                tree.set_bounds(t.target_bar_id, target_bar_rect(track_rect, norm));
            }
            let pid = self.rows[pi].id.clone();
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Effect(ei), pid), ScrubPhase::Move(ScrubValue::Scalar(norm)))],
                ParamCardKind::Generator => vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Generator, pid), ScrubPhase::Move(ScrubValue::Scalar(norm)))],
            };
        }

        // Envelope decay slider drag — update the drawer slider's fill + value,
        // dispatch the decay change (in beats).
        if let Some(pi) = self.drag.env_decay_index()
            && let Some(cfg) = self.row_host.envelope_config_ids.get(pi).and_then(|c| c.as_ref())
        {
            let norm = BitmapSlider::x_to_normalized(
                TrackSpan::of(tree.get_bounds(cfg.decay_slider.track)),
                pos.x,
            )
            .clamp(0.0, 1.0);
            let decay = norm * ENV_DECAY_MAX;
            if let Some(v) = self.state.mod_state.env_decay.get_mut(pi) {
                *v = decay;
            }
            BitmapSlider::update_value(tree, &cfg.decay_slider, norm, &format!("{decay:.2}"));
            let pid = self.rows[pi].id.clone();
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(ei), pid), ScrubPhase::Move(ScrubValue::Scalar(decay)))],
                ParamCardKind::Generator => vec![PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Generator, pid), ScrubPhase::Move(ScrubValue::Scalar(decay)))],
            };
        }

        // Audio shaping slider drag — update fill + value, dispatch live edit.
        if let Some((pi, which)) = self.drag.audio_shape() {
            let si = match which {
                AudioShapeParam::Sensitivity => 0,
                AudioShapeParam::Attack => 1,
                AudioShapeParam::Release => 2,
            };
            let track_id = self.row_host
                .audio_configs
                .get(pi)
                .and_then(|c| c.as_ref())
                .and_then(|(d, _)| d.sliders.get(si))
                .map(|sl| sl.track);
            if let Some(track_id) = track_id {
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(track_id)),
                    pos.x,
                )
                .clamp(0.0, 1.0);
                let value = audio_shape_value_from_norm(which, norm);
                match which {
                    AudioShapeParam::Sensitivity => {
                        if let Some(v) = self.state.mod_state.audio_sensitivity.get_mut(pi) {
                            *v = value;
                        }
                    }
                    AudioShapeParam::Attack => {
                        if let Some(v) = self.state.mod_state.audio_attack_ms.get_mut(pi) {
                            *v = value;
                        }
                    }
                    AudioShapeParam::Release => {
                        if let Some(v) = self.state.mod_state.audio_release_ms.get_mut(pi) {
                            *v = value;
                        }
                    }
                }
                let text = audio_shape_value_text(which, value);
                if let Some((d, _)) = self.row_host.audio_configs.get(pi).and_then(|c| c.as_ref())
                    && let Some(sl) = d.sliders.get(si)
                {
                    BitmapSlider::update_value(tree, sl, norm, &text);
                }
                let pid = self.rows[pi].id.clone();
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(
                        ValueRef::AudioModShape(GraphParamTarget::Effect(ei), pid, which),
                        ScrubPhase::Move(ScrubValue::Scalar(value)),
                    )],
                    ParamCardKind::Generator => vec![PanelAction::Scrub(
                        ValueRef::AudioModShape(GraphParamTarget::Generator, pid, which),
                        ScrubPhase::Move(ScrubValue::Scalar(value)),
                    )],
                };
            }
        }

        // Step-Amount slider drag (D8) — its own path, see `handle_pointer_down`
        // 2c. Updates fill + value and dispatches the live edit.
        if let Some(pi) = self.drag.step_amount() {
            let track_id = self.row_host
                .audio_configs
                .get(pi)
                .and_then(|c| c.as_ref())
                .and_then(|(d, _)| d.sliders.get(3))
                .map(|sl| sl.track);
            if let Some(track_id) = track_id {
                let info = &self.rows[pi];
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(track_id)),
                    pos.x,
                )
                .clamp(0.0, 1.0);
                let mut value = norm_to_step_amount(norm, info.spec.min, info.spec.max);
                if info.spec.whole_numbers {
                    value = value.round();
                }
                if let Some(v) = self.state.mod_state.audio_step_amount.get_mut(pi) {
                    *v = value;
                }
                let text =
                    if info.spec.whole_numbers { format!("{value:.0}") } else { format!("{value:.2}") };
                let display_norm = step_amount_to_norm(value, info.spec.min, info.spec.max);
                if let Some((d, _)) = self.row_host.audio_configs.get(pi).and_then(|c| c.as_ref())
                    && let Some(sl) = d.sliders.get(3)
                {
                    BitmapSlider::update_value(tree, sl, display_norm, &text);
                }
                let pid = self.rows[pi].id.clone();
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(ei), pid), ScrubPhase::Move(ScrubValue::Scalar(value)))]
                    }
                    ParamCardKind::Generator => vec![PanelAction::Scrub(
                        ValueRef::AudioModStepAmount(GraphParamTarget::Generator, pid),
                        ScrubPhase::Move(ScrubValue::Scalar(value)),
                    )],
                };
            }
        }

        // Trim bar drag (driver / Ableton / audio) — one path. Read the kind's
        // current range, clamp the dragged edge, write it back, reposition the
        // bars, emit the change. The clamp and `reposition_trim_bars` are
        // identical across kinds (`x_to_normalized` pre-clamps to [0,1], so the
        // old `norm.min`/`norm.clamp` spellings coincide); only the backing
        // store differs, and `TrimKind` selects it via the trim accessors.
        if let Some((kind, pi, is_min)) = self.drag.trim()
            && let Some(track_id) = self.row_host
                .slider_ids
                .get(pi)
                .and_then(|s| s.as_ref())
                .map(|s| s.track)
            && let Some((cur_min, cur_max)) = self.trim_range(kind, pi)
        {
            // Live bounds, not the cached `track_rect`: in-place scroll shifts
            // the tree nodes without refreshing the cache, and feeding its
            // stale y to `reposition_trim_bars` teleports the bars off the
            // slider (BUG-257).
            let track_rect = tree.get_bounds(track_id);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos.x);
            let (new_min, new_max) = if is_min {
                (norm.min(cur_max), cur_max)
            } else {
                (cur_min, norm.max(cur_min))
            };
            self.set_trim_range(kind, pi, new_min, new_max);

            // Visual update: reposition this kind's trim bar nodes in the tree.
            if let Some(t) = self.trim_ids_for(kind).get(pi).and_then(|t| t.as_ref()).copied() {
                super::super::param_slider_shared::reposition_trim_bars(
                    tree, track_rect, &t, new_min, new_max,
                );
            }

            let pid = self.rows[pi].id.clone();
            return match self.kind {
                ParamCardKind::Effect => {
                    vec![PanelAction::Scrub(ValueRef::Trim(kind, GraphParamTarget::Effect(ei), pid), ScrubPhase::Move(ScrubValue::Range(new_min, new_max)))]
                }
                ParamCardKind::Generator => {
                    vec![PanelAction::Scrub(ValueRef::Trim(kind, GraphParamTarget::Generator, pid), ScrubPhase::Move(ScrubValue::Range(new_min, new_max)))]
                }
            };
        }

        // Param slider drag
        if let Some(pi) = self.drag.param_index()
            && let Some(ids) = self.row_host.slider_ids.get(pi).and_then(|s| s.as_ref())
        {
            let info = &self.rows[pi];
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
            let val = BitmapSlider::normalized_to_value(norm, info.spec.min, info.spec.max);
            let val = if info.spec.whole_numbers { val.round() } else { val };
            let display_norm = BitmapSlider::value_to_normalized(val, info.spec.min, info.spec.max);
            let text = format_param_value(
                val,
                info.spec.min,
                info.spec.whole_numbers,
                info.spec.is_angle,
                info.spec.value_labels.as_deref(),
            );
            BitmapSlider::update_value(tree, ids, display_norm, &text);
            self.param_cache[pi] = val;
            let pid = self.rows[pi].id.clone();
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::Scrub(
                    ValueRef::Param(GraphParamTarget::Effect(ei), pid),
                    ScrubPhase::Move(ScrubValue::Scalar(val)),
                )],
                ParamCardKind::Generator => vec![PanelAction::Scrub(
                    ValueRef::Param(GraphParamTarget::Generator, pid),
                    ScrubPhase::Move(ScrubValue::Scalar(val)),
                )],
            };
        }

        // D3 relight-knob drag (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) — mirrors
        // the plain param-slider drag above exactly, minus the value cache
        // (relight knobs have no per-row `rows`/`param_cache` slot).
        if let Some(field) = self.drag.relight_field()
            && let Some(i) = RELIGHT_FIELD_SPECS.iter().position(|s| s.field == field)
            && let Some(ids) = self.relight_slider_ids[i].as_ref()
        {
            let spec = &RELIGHT_FIELD_SPECS[i];
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
            let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
            let display_norm = BitmapSlider::value_to_normalized(val, spec.min, spec.max);
            self.relight.set_value(field, val);
            BitmapSlider::update_value(tree, ids, display_norm, &format!("{val:.2}"));
            return vec![PanelAction::Scrub(ValueRef::RelightParam(self.param_target(), field), ScrubPhase::Move(ScrubValue::Scalar(val)))];
        }

        Vec::new()
    }

    /// Drag-end dispatch — commit the active drag. Identical bookkeeping for
    /// both kinds; only the emitted [`PanelAction`] variant differs.
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        match self.drag.end() {
            Some(ParamDragTarget::EnvTarget { index: pi }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Effect(ei), pid), ScrubPhase::Commit)],
                    ParamCardKind::Generator => vec![PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Generator, pid), ScrubPhase::Commit)],
                }
            }
            Some(ParamDragTarget::EnvDecay { index: pi }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(ei), pid), ScrubPhase::Commit)],
                    ParamCardKind::Generator => vec![PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Generator, pid), ScrubPhase::Commit)],
                }
            }
            Some(ParamDragTarget::AudioShape { index: pi, param: which }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Effect(ei), pid, which), ScrubPhase::Commit)],
                    ParamCardKind::Generator => {
                        vec![PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Generator, pid, which), ScrubPhase::Commit)]
                    }
                }
            }
            Some(ParamDragTarget::StepAmount { index: pi }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(ei), pid), ScrubPhase::Commit)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Generator, pid), ScrubPhase::Commit)]
                    }
                }
            }
            Some(ParamDragTarget::Trim { kind, index: pi, .. }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(ValueRef::Trim(kind, GraphParamTarget::Effect(ei), pid), ScrubPhase::Commit)],
                    ParamCardKind::Generator => {
                        vec![PanelAction::Scrub(ValueRef::Trim(kind, GraphParamTarget::Generator, pid), ScrubPhase::Commit)]
                    }
                }
            }
            Some(ParamDragTarget::Param { index: pi }) => {
                let pid = self.rows[pi].id.clone();
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::Scrub(
                        ValueRef::Param(GraphParamTarget::Effect(ei), pid),
                        ScrubPhase::Commit,
                    )],
                    ParamCardKind::Generator => vec![PanelAction::Scrub(
                        ValueRef::Param(GraphParamTarget::Generator, pid),
                        ScrubPhase::Commit,
                    )],
                }
            }
            Some(ParamDragTarget::Relight { field }) => {
                vec![PanelAction::Scrub(ValueRef::RelightParam(self.param_target(), field), ScrubPhase::Commit)]
            }
            None => Vec::new(),
        }
    }

    /// Node-intent dispatch for this card's right-click gestures. The sole
    /// right-click path for both the inspector and the graph-editor card.
    /// Declarative intent + fold-up: specific intents on the slider track
    /// (reset) and label (perform mapping) win, and the card root claims its
    /// whole area so a right-click on any dead zone — slider fill/thumb/value
    /// cell, row gaps, padding — folds up to the card context menu instead of
    /// being silently swallowed. See `docs/NODE_INTENT_DISPATCH.md`.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        if !self.is_live() {
            return;
        }
        use crate::intent::Gesture::RightClick;

        let target = match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        };

        // Card root: claim the whole area + the context-menu action. Any
        // descendant without a more specific intent folds here.
        if let Some(border_id) = self.border_id {
            intents.claim_area(border_id);
            intents.on(border_id, RightClick, PanelAction::Params(ParamsAction::CardRightClicked(target.clone())));
        }

        // Every materialised slider's right-click reset — main rows AND every
        // drawer slider (audio-shape Amount/Attack/Release, envelope Decay) —
        // replayed independent of row kind (slider / toggle / trigger /
        // trigger-gate). This is what fixes BUG-070: a trigger-gate row has no
        // main slider, but its armed drawer's sliders are stored in
        // `audio_configs[pi]` regardless, so this pass reaches them directly
        // instead of piggybacking on the main-slider loop below. The row-level
        // reset replay lives on `RowHost` (it owns those id bundles); the panel
        // adds the card-chrome intents (border claim above, relight resets +
        // per-param mapping menus below).
        self.row_host.register_intents(intents);
        // D3 relight-knob resets (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) — same
        // pattern as the main-row loop above.
        for (ids, reset) in self.relight_slider_ids.iter().zip(self.relight_slider_resets.iter()) {
            if let (Some(ids), Some(reset)) = (ids, reset) {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }

        // Per-param perform-mapping menu.
        for (pi, slider) in self.row_host.slider_ids.iter().enumerate() {
            // Generator toggle/trigger rows have no map gesture — they fall
            // through to the card claim like any other dead zone.
            if matches!(self.kind, ParamCardKind::Generator)
                && self
                    .rows
                    .get(pi)
                    .map(|i| i.spec.is_toggle || i.spec.is_trigger)
                    .unwrap_or(false)
            {
                continue;
            }
            let Some(ids) = slider else { continue };

            // Rest of the row → perform-mapping menu (Perform context only;
            // Author uses the right-edge mapping drawer instead). Registered on
            // both the interactive label and the full-row catcher behind the
            // value cell + gaps, so a right-click anywhere on the row that isn't
            // the track reliably opens the param menu — no narrow-target lottery.
            if self.context == CardContext::Perform {
                let menu = PanelAction::Params(ParamsAction::ParamLabelRightClick(target.clone(), self.rows[pi].id.clone()));
                // Label registration goes through the contract (P3/D14).
                BitmapSlider::register_label_mapping(ids, &menu, intents);
                // The row catcher is a second node carrying the SAME action
                // — host-attached chrome, not a contract zone (it's a
                // full-row dead-zone catcher behind the value cell + gaps,
                // no `SliderZone` of its own), so it stays hand-registered.
                if let Some(Some(catcher)) = self.row_host.row_catcher_ids.get(pi).copied() {
                    intents.claim_area(catcher);
                    intents.on(catcher, RightClick, menu.clone());
                }
                // The value cell carries the same menu: it wins the hit-test
                // over the catcher (BUG-250's fix made it interactive per its
                // zone contract), and `ValueCell + RightClick` is a contract
                // dead stop hosts may bind (D13) — binding it keeps the
                // pre-fix "right-click anywhere off-track opens the menu"
                // behavior instead of degrading to the card menu.
                intents.on(ids.value_text, RightClick, menu);
            }
        }
    }
}
