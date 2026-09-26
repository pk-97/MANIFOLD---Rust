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
                // SCENE_MODIFIER_FRAMEWORK section 3.7 modifier chrome — the
                // kind-specific buttons precede the shared effect shell.
                if let Some(m) = &self.modifier {
                    // Graph-editor navigation — the same destination the
                    // generator card's cog emits: a modifier's nodes live in
                    // the layer's generator graph, so that is where the cog
                    // goes. Must precede the shared Effect arm below, which
                    // would emit OpenGraphEditor(0) — modifier surfaces are
                    // Effect-kind with a placeholder index.
                    if self.cog_btn_id == Some(id) {
                        return vec![PanelAction::Root(RootAction::OpenGraphTarget(
                            crate::view::UiGraphTarget::SceneModifier {
                                owner: Box::new(crate::view::UiGraphTarget::Generator(
                                    m.layer_id.clone(),
                                )),
                                modifier_id: m.instance_id.clone(),
                            },
                        ))];
                    }
                    if self.toggle_btn_id == Some(id) {
                        // Common modifier enable toggle: ONE param write on
                        // the descriptor's enable target (INV-M7), resolved
                        // app-side.
                        return vec![PanelAction::Project(
                            crate::panels::ProjectAction::SceneModifierToggleEnabled(
                                m.layer_id.clone(),
                                m.instance_id.clone(),
                            ),
                        )];
                    }
                    if self.modifier_objects_btn_id == Some(id) {
                        return vec![PanelAction::Root(RootAction::SceneModifierObjectsClicked(
                            m.layer_id.clone(),
                            m.instance_id.clone(),
                        ))];
                    }
                    if self.modifier_remove_btn_id == Some(id) {
                        // The delete-collapse exit animation rides the
                        // inspector's reconcile machinery — this only mutates.
                        return vec![PanelAction::Project(
                            crate::panels::ProjectAction::SceneModifiersRemove(
                                m.layer_id.clone(),
                                vec![m.instance_id.clone()],
                            ),
                        )];
                    }
                    if self.chevron_btn_id == Some(id) {
                        self.set_collapsed(!self.is_collapsed());
                        return vec![PanelAction::Params(ParamsAction::SectionFoldToggled)];
                    }
                    if self.header_bg_id == Some(id)
                        || self.name_label_id == Some(id)
                        || self.border_id == Some(id)
                    {
                        return vec![PanelAction::Params(ParamsAction::ModifierCardClicked(
                            m.instance_id.clone(),
                        ))];
                    }
                }
                if let Some(m) = &self.object_modifier {
                    if self.chevron_btn_id == Some(id) {
                        self.set_collapsed(!self.is_collapsed());
                        return vec![PanelAction::Params(ParamsAction::SectionFoldToggled)];
                    }
                    if self.cog_btn_id == Some(id) {
                        return vec![PanelAction::Root(RootAction::SceneSetupOpenGraphEditor(
                            m.layer_id.clone(),
                        ))];
                    }
                    if self.modifier_remove_btn_id == Some(id) {
                        return vec![PanelAction::Project(
                            crate::panels::ProjectAction::SceneSetupRemoveModifier(
                                m.layer_id.clone(),
                                m.group_node_id.unwrap_or(m.object_id),
                                m.node_doc_id,
                            ),
                        )];
                    }
                    if self.header_bg_id == Some(id)
                        || self.name_label_id == Some(id)
                        || self.border_id == Some(id)
                    {
                        // Scene Setup owns selection for object cards. The
                        // host captures the stable address on pointer/click;
                        // no inspector modifier/effect selection action is
                        // valid for this card species.
                        return Vec::new();
                    }
                }
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
                for (si, &btn_id) in self.string_param_btn_ids.iter().enumerate() {
                    if btn_id == Some(id)
                        && let Some(sp) = self.string_param_info.get(si)
                        && sp.use_dropdown
                        && let (Some(effect_id), Some(binding_id)) =
                            (&sp.effect_id, &sp.binding_id)
                    {
                        return vec![PanelAction::Params(
                            ParamsAction::EffectStringParamDropdownClicked(
                                effect_id.clone(),
                                binding_id.clone(),
                                si,
                            ),
                        )];
                    }
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
            // A projection-disabled row (`RowSpec.disabled`) is a dead click:
            // no toggle, no drawer, no OSC copy. The greyed label says why.
            if self.rows.get(row).is_some_and(|r| r.spec.disabled.is_some()) {
                return Vec::new();
            }
            // SCENE_MODIFIER_FRAMEWORK D4: a modifier card's toggle row is a
            // REAL scene write — one undoable param write through
            // `SceneSetupParamChanged` (INV-M7), addressed by the row's
            // ParamAddr sidecar, never the plain manifest-param toggle wire.
            if role == RowRole::ToggleBtn
                && !self.rows[row].spec.is_trigger
                && let (Some(m), Some(addr)) = (&self.modifier, &self.rows[row].scene_addr)
            {
                let current = self.rows[row].value.base;
                let next = if current > 0.5 { 0.0 } else { 1.0 };
                return vec![PanelAction::Project(
                    crate::panels::ProjectAction::SceneSetupParamChanged(
                        m.layer_id.clone(),
                        addr.scope_path.clone(),
                        addr.node_doc_id,
                        addr.param_id.clone(),
                        next,
                    ),
                )];
            }
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
        self.relight_drag.cancel();
        let target = self.param_target();
        let row_actions = self.row_host.handle_pointer_down(
            node_id,
            pos,
            tree,
            RowInteraction {
                target: &target,
                rows: &mut self.rows,
                modulation: &mut self.state.mod_state,
                values: &mut self.param_cache,
                row_indices: &self.row_id_index,
            },
        );
        if !row_actions.is_empty() {
            return row_actions;
        }

        // Relight is card chrome rather than a parameter row. Its address and
        // field are captured so a layer/selection change cannot retarget the
        // gesture while it is in flight.
        for (slider, spec) in self.relight_slider_ids.iter().zip(RELIGHT_FIELD_SPECS.iter()) {
            if let Some(ids) = slider
                && node_id == ids.track
            {
                let field = spec.field;
                self.row_host.cancel_drag();
                let begin = self.relight_drag.begin(
                    ValueRef::RelightParam(target.clone(), field),
                    field,
                    pos,
                );
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(ids.track)),
                    pos.x,
                );
                let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
                let moved = self.relight_drag.update(pos, ScrubValue::Scalar(val));
                self.relight.set_value(field, val);
                return [Some(begin), moved].into_iter().flatten().collect();
            }
        }
        Vec::new()
    }

    /// Drag-move dispatch. The state mutation + tree repositioning and the
    /// emitted [`PanelAction`]s are identical for both kinds — every emission
    /// rides `param_target()` (BUG-3jpj: a modifier card's kind is Effect with
    /// effect_index 0, so deriving the target from `self.kind` here re-addresses
    /// mid-gesture moves to effect 0 and the drag dies). `fine` is Shift-held,
    /// sampled per move (not just at grab) so pressing/releasing Shift mid-drag
    /// re-scales sensitivity live (D8).
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree, fine: bool) -> Vec<PanelAction> {
        if self.row_host.is_dragging() {
            let target = self.param_target();
            return self.row_host.handle_drag(
                pos,
                tree,
                fine,
                RowInteraction {
                    target: &target,
                    rows: &mut self.rows,
                    modulation: &mut self.state.mod_state,
                    values: &mut self.param_cache,
                    row_indices: &self.row_id_index,
                },
            );
        }

        if let Some(&field) = self.relight_drag.payload()
            && let Some(i) = RELIGHT_FIELD_SPECS.iter().position(|s| s.field == field)
            && let Some(ids) = self.relight_slider_ids[i].as_ref()
        {
            let current_address = ValueRef::RelightParam(self.param_target(), field);
            if self.relight_drag.address() != Some(&current_address) {
                return Vec::new();
            }
            let spec = &RELIGHT_FIELD_SPECS[i];
            let norm = BitmapSlider::x_to_normalized(
                TrackSpan::of(tree.get_bounds(ids.track)),
                pos.x,
            );
            let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
            self.relight.set_value(field, val);
            let display_norm = BitmapSlider::value_to_normalized(val, spec.min, spec.max);
            BitmapSlider::update_value(tree, ids, display_norm, &format!("{val:.2}"));
            return self
                .relight_drag
                .update(pos, ScrubValue::Scalar(val))
                .into_iter()
                .collect();
        }

        Vec::new()
    }

    /// Drag-end dispatch — commit the active drag. Identical bookkeeping for
    /// both kinds; every emission rides `param_target()` (BUG-3jpj — see
    /// `handle_drag`).
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.row_host.is_dragging() {
            return self.row_host.handle_drag_end();
        }
        self.relight_drag.end().into_iter().collect()
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

        // Every emission rides param_target() — a modifier card must address
        // its owning layer here too (BUG-3jpj), not effect_index 0.
        let target = self.param_target();

        // Card root: claim the whole area + the context-menu action. Any
        // descendant without a more specific intent folds here.
        if let Some(border_id) = self.border_id {
            intents.claim_area(border_id);
            let menu = if let Some(m) = &self.modifier {
                PanelAction::Root(RootAction::SceneModifierCardRightClicked(
                    m.layer_id.clone(),
                    m.instance_id.clone(),
                ))
            } else if let Some(m) = &self.object_modifier {
                PanelAction::Root(RootAction::ObjectModifierCardRightClicked(m.clone()))
            } else {
                PanelAction::Params(ParamsAction::CardRightClicked(target.clone()))
            };
            intents.on(border_id, RightClick, menu);
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

        // AUTO opens the same parameter menu as the label, including clear.
        for (pi, button) in self.row_host.automation_btn_ids.iter().enumerate() {
            if let Some(button) = button {
                intents.on(*button, RightClick, PanelAction::Params(
                    ParamsAction::ParamLabelRightClick(target.clone(), self.rows[pi].id.clone()),
                ));
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

            // Rest of the row → parameter menu, including Show Automation.
            // Author and Perform share this explicit lane-entry action. Registered on
            // both the interactive label and the full-row catcher behind the
            // value cell + gaps, so a right-click anywhere on the row that isn't
            // the track reliably opens the param menu — no narrow-target lottery.
            {
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
