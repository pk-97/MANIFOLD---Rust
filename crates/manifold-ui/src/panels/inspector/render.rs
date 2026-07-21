use super::*;

impl InspectorCompositePanel {
    /// Set which tab rungs are available (display order, local→global) and which
    /// is active. Drives section visibility so only the active scope renders.
    pub fn configure_tabs(&mut self, available: &[InspectorTab], active: InspectorTab) {
        self.available_tabs.clear();
        self.available_tabs.extend_from_slice(available);
        self.set_active_tab(active);
    }

    /// Build the tab strip: one button per available rung, the active one
    /// highlighted. Records node ids for click routing.
    fn build_tab_strip(&mut self, tree: &mut UITree, rect: Rect) {
        self.tab_node_ids.clear();
        self.collapse_all_btn_id = None;
        self.compact_toggle_btn_id = None;
        if self.available_tabs.is_empty() {
            return;
        }
        let n = self.available_tabs.len();
        // The per-column controls (compact toggle + collapse-all) are anchored to
        // the strip's RIGHT EDGE — a fixed position regardless of which tab is
        // active, so they don't slide when selection changes. The tabs lay out in
        // the remaining width on the left. The block claims `controls_extra` at the
        // right: [gap][cog][gap][collapse]. Hidden when the active column has no
        // cards (then the tabs span the full width).
        let show_controls = self.active_column_card_count() > 0;
        let controls_extra = if show_controls {
            TAB_GAP + COMPACT_TOGGLE_W + TAB_GAP + COLLAPSE_ALL_W
        } else {
            0.0
        };
        let tab_area_w = (rect.width - controls_extra).max(1.0);
        let inter_gap = TAB_GAP * n.saturating_sub(1) as f32;
        let tab_w = ((tab_area_w - inter_gap) / n as f32).floor().max(1.0);

        let tabs = self.available_tabs.clone();
        let mut x = rect.x;
        for (idx, tab) in tabs.iter().enumerate() {
            if idx > 0 {
                x += TAB_GAP;
            }
            let active = *tab == self.active_tab;
            // The kit segmented-control cell — the Clip/Layer/Master tabs and any
            // other tab strip share one look.
            let mut style = UIStyle {
                font_size: TAB_FONT_SIZE,
                ..chrome::components::segment_style(active)
            };
            // Tint the SELECTED tab toward the one inspector accent (not a lane
            // hue), so the active scope reads as selected without per-layer colour.
            if active {
                style.bg_color = color::mix(color::BG_2, color::INSPECTOR_ACCENT, 0.30);
            }
            // Interactive button (not a label) so clicks hit-test and route —
            // a plain label carries no INTERACTIVE flag and is invisible to the
            // event system, which is why the tabs were unclickable.
            let id = tree.add_button(None, x, rect.y, tab_w, rect.height, style, Self::tab_label(*tab));
            self.tab_node_ids.push((id, *tab));
            x += tab_w;
        }

        // Controls anchored to the strip's right edge — fixed, independent of the
        // active tab. cog_x is back-computed so the collapse button's right edge
        // lands flush at `rect.right`.
        if show_controls {
            let cog_x = rect.x + rect.width - COLLAPSE_ALL_W - TAB_GAP - COMPACT_TOGGLE_W;
            self.build_tab_controls(tree, cog_x, rect.y, rect.height);
        }
    }

    /// Build the active tab's per-column controls (compact toggle + collapse-all),
    /// laid out left→right starting at `x`. Returns the x after the last control.
    /// They act on the active tab's column (the single source of truth).
    fn build_tab_controls(&mut self, tree: &mut UITree, x: f32, y: f32, h: f32) -> f32 {
        // §6b — compact toggle (cog): hide every card's modulation config drawers
        // while keeping mods armed. The kit toggle — accent fill when engaged.
        let id = tree.add_button(
            None,
            x,
            y,
            COMPACT_TOGGLE_W,
            h,
            UIStyle {
                font_size: color::FONT_BODY,
                ..chrome::components::toggle_style(self.mods_compact)
            },
            // cog (atlas icon) — hide/show modulation settings
            &crate::icons::Icon::Cog.text(),
        );
        self.compact_toggle_btn_id = Some(id);

        // Collapse-all / expand-all. Label reflects the action it will take:
        // "Collapse" while any card is open, else "Expand".
        let x = x + COMPACT_TOGGLE_W + TAB_GAP;
        let any_expanded = self.any_active_card_expanded();
        let id = tree.add_button(
            None,
            x,
            y,
            COLLAPSE_ALL_W,
            h,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_BODY,
                ..chrome::components::button_secondary_style()
            },
            if any_expanded { "Collapse" } else { "Expand" },
        );
        self.collapse_all_btn_id = Some(id);
        x + COLLAPSE_ALL_W
    }

    /// Sub-region node ranges for incremental cache re-rendering.
    /// Returns (node_start, node_end) for each sub-panel: chrome panels,
    /// effect cards, gen params. Used by the cache manager to detect which
    /// parts of the inspector changed and only re-render those.
    ///
    /// Every sub-panel is offered, but only the ones that built nodes this frame
    /// contribute a range: an inactive scope was reset to an empty range in
    /// `build`, and `push` drops `(usize::MAX, 0)` — so the cache is fed exactly
    /// what was actually built. No `*_visible()` gate; the node range is the
    /// single source of truth for "live this frame".
    pub fn sub_region_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = Vec::with_capacity(
            4 + self.effects[Self::SCOPE_MASTER].len() + self.effects[Self::SCOPE_LAYER].len() + 1,
        );
        let push = |ranges: &mut Vec<(usize, usize)>, first: usize, count: usize| {
            if first != usize::MAX && count > 0 {
                ranges.push((first, first + count));
            }
        };
        push(
            &mut ranges,
            self.macros_panel.first_node(),
            self.macros_panel.node_count(),
        );
        push(
            &mut ranges,
            self.master_chrome.first_node(),
            self.master_chrome.node_count(),
        );
        for card in &self.effects[Self::SCOPE_MASTER] {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        push(
            &mut ranges,
            self.layer_chrome.first_node(),
            self.layer_chrome.node_count(),
        );
        if let Some(ref gp) = self.gen_params {
            push(&mut ranges, gp.first_node(), gp.node_count());
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        push(
            &mut ranges,
            self.clip_chrome.first_node(),
            self.clip_chrome.node_count(),
        );
        ranges
    }

    pub fn configure_master_effects(&mut self, configs: &[ParamSurface]) {
        let existing = std::mem::take(&mut self.effects[Self::SCOPE_MASTER]);
        self.effects[Self::SCOPE_MASTER] =
            Self::reconcile_cards(existing, configs, &mut self.master_dying, self.card_context);
    }

    pub fn configure_layer_effects(&mut self, configs: &[ParamSurface], scope: Option<&LayerId>) {
        // A change of scope is navigation, not an edit of the current chain:
        // the previously-shown layer's effects weren't removed from the model,
        // so they must not play the delete-collapse exit animation.
        // `reconcile_cards` can't tell the difference — on a switch none of the
        // old cards match the new layer's effect IDs, so it would move every
        // one of them into `layer_dying` and the whole stale chain would linger
        // mid-collapse over the new selection. Drop them instantly instead, and
        // abandon any in-flight death carried over from the old scope.
        if scope != self.layer_scope_id.as_ref() {
            self.effects[Self::SCOPE_LAYER].clear();
            self.layer_dying.clear();
            self.layer_scope_id = scope.cloned();
        }
        let existing = std::mem::take(&mut self.effects[Self::SCOPE_LAYER]);
        self.effects[Self::SCOPE_LAYER] =
            Self::reconcile_cards(existing, configs, &mut self.layer_dying, self.card_context);
    }

    pub fn configure_gen_params(
        &mut self,
        config: Option<&ParamSurface>,
        layer_id: Option<LayerId>,
    ) {
        // The generator card is a single optional, distinct from the effect
        // lists (it carries no EffectId and is outside the selection +
        // drag-reorder model). Reuse the existing panel when the selection still
        // points at the same layer's generator, so its transient UI state (the
        // modulation config tab) survives the rebuild. `set_layer_id` is applied
        // before `configure` per its contract.
        self.gen_params = config.map(|cfg| {
            let reused = self
                .gen_params
                .take()
                .filter(|p| p.owning_layer_id() == layer_id.as_ref());
            let mut panel = reused.unwrap_or_default();
            panel.set_context(self.card_context);
            panel.set_layer_id(layer_id);
            panel.configure(cfg);
            panel
        });
    }

    /// Set the chrome context applied to every card this panel owns —
    /// `CardContext::Author` for the graph-editor window's inspector
    /// instance, `CardContext::Perform` (the default) for the main window's.
    /// Set once by the host at construction (mirrors
    /// `ParamCardPanel::set_context`'s doc comment); applies immediately to
    /// every card currently held (including dying ones, so an in-flight
    /// collapse doesn't flash back to Perform chrome) and every card built
    /// afterward via `reconcile_cards` / `configure_gen_params`.
    pub fn set_card_context(&mut self, context: CardContext) {
        self.card_context = context;
        for card in self
            .effects
            .iter_mut()
            .flatten()
            .chain(self.gen_params.iter_mut())
            .chain(self.master_dying.iter_mut())
            .chain(self.layer_dying.iter_mut())
        {
            card.set_context(context);
        }
    }

    /// Reconcile the existing card panels against the new configs, **reusing** a
    /// panel whose effect identity matches so its transient UI-only state (the
    /// modulation config tab, drag, copy-flash) survives the per-snapshot
    /// rebuild instead of being thrown away. The result is in config (effect)
    /// order.
    ///
    /// D17 additions on top of the original reuse mechanism: a config with NO
    /// matching existing panel is genuinely new — its fresh panel fires the
    /// "spawn pop" scale-in (`ParamCardPanel::fire_spawn_pop`). An existing
    /// panel with no matching config anymore was just removed from the model;
    /// instead of dropping it here, it moves into `dying` — the exit-state
    /// pattern (`anim.rs`'s doc comment) — so the caller keeps drawing it
    /// through its collapse+fade instead of it vanishing mid-frame.
    ///
    /// Replaces the old build-fresh-every-frame path, which reset transient UI
    /// state every sync and re-allocated every panel each frame.
    fn reconcile_cards(
        mut existing: Vec<ParamCardPanel>,
        configs: &[ParamSurface],
        dying: &mut Vec<ParamCardPanel>,
        card_context: CardContext,
    ) -> Vec<ParamCardPanel> {
        let reconciled = configs
            .iter()
            .map(|cfg| match existing.iter().position(|c| c.matches_effect_config(cfg)) {
                Some(pos) => {
                    let mut card = existing.remove(pos);
                    card.configure(cfg);
                    card
                }
                None => {
                    let mut card = ParamCardPanel::default();
                    card.set_context(card_context);
                    card.configure(cfg);
                    card.fire_spawn_pop();
                    card
                }
            })
            .collect();
        // Whatever's left in `existing` no longer matches any config — it was
        // removed from the model this rebuild. Keep it alive in `dying`
        // rather than letting it drop here.
        for mut card in existing {
            card.begin_delete_collapse();
            dying.push(card);
        }
        reconciled
    }

    /// Content height for the master column (left).
    pub(super) fn master_column_height(&self) -> f32 {
        if !self.master_visible() {
            return 0.0;
        }
        let mut h = SECTION_CARD_PAD + self.master_chrome.compute_height();
        if !self.master_chrome.is_collapsed() {
            for card in &self.effects[Self::SCOPE_MASTER] {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP;
        }
        h + SECTION_CARD_PAD
    }

    /// Content height for the layer column (right).
    /// Order: layer chrome → AUDIO TRIGGERS (P3b) → gen params → layer
    /// effects → add effect button.
    fn layer_column_height(&self) -> f32 {
        let mut h = 0.0;
        if self.layer_visible() {
            h += SECTION_CARD_PAD + self.layer_chrome.compute_height();
            if !self.layer_chrome.is_collapsed() {
                // AUDIO TRIGGERS sits at the top of the layer's detail
                // content — above gen params and layer effects.
                h += self.audio_trigger_section.height() + SECTION_GAP;
                // Gen params sit above layer effects
                if let Some(ref gp) = self.gen_params {
                    h += gp.compute_height() + SECTION_GAP;
                }
                for card in &self.effects[Self::SCOPE_LAYER] {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_CARD_PAD + SECTION_GAP;
        }
        h
    }

    /// Total scrollable content height for the right (Layer + Clip) column.
    pub(super) fn right_column_height(&self) -> f32 {
        self.layer_column_height() + self.clip_section_height()
    }

    /// Build the whole inspector column into an explicit `rect`, decoupled from
    /// `ScreenLayout::inspector()` so the graph-editor window can host the same
    /// column in its right lane (`dock.right`). `Panel::build` is the thin
    /// wrapper that passes `layout.inspector()`.
    pub fn build_in_rect(&mut self, tree: &mut UITree, rect: Rect) {
        self.cache_first_node = tree.count();

        // Add-effect button ids are reassigned by node index every rebuild, but
        // each is only *set* inside its `!collapsed` build branch. Clear them up
        // front so a collapsed/hidden section can't leave a stale id pointing at
        // an index another node now occupies — e.g. the generator card's Change
        // button inheriting a stale add-layer-effect id and opening the effect
        // browser instead of the generator picker. (The exact-id checks in
        // handle_click run before the range-based find_target_for_node.)
        self.add_master_effect_btn = None;
        self.add_layer_effect_btn = None;

        // Range truthfulness (the single invariant the rest of this panel leans
        // on): a sub-panel's (first_node, node_count) must describe what it built
        // THIS frame. Only the active scope's sections build below, so reset every
        // section's range up front — a section left un-built then honestly reports
        // an empty range. `node_count() > 0` ("live this frame") becomes the one
        // signal every consumer keys off (hit routing, intents, type-in, the
        // sub-region cache, selection visuals), so an inactive scope can never
        // alias the active scope's node indices. This is what makes the tab system
        // safe by construction instead of by a `*_visible()` guard repeated at
        // every read site. Runs before the zero-width early-return so a collapsed
        // inspector also leaves no stale ranges.
        self.master_chrome.clear_nodes();
        self.layer_chrome.clear_nodes();
        self.clip_chrome.clear_nodes();
        self.audio_trigger_section.clear_nodes();
        if let Some(gp) = self.gen_params.as_mut() {
            gp.clear_nodes();
        }
        for card in self.effects.iter_mut().flatten() {
            card.clear_nodes();
        }

        if rect.width <= 0.0 {
            return;
        }

        self.viewport_rect = rect;

        // §6b — propagate global compact mode to every card before layout, so
        // compute_height and build agree on whether drawers are hidden.
        self.apply_mods_compact();

        // Background panel
        self.bg_panel_id = Some(tree.add_panel(
            None,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                ..UIStyle::default()
            },
        ));

        // Macros strip pinned to the very top of the inspector. The macro bank is
        // a global (project-level) control, so it sits ABOVE the per-scope tab
        // strip rather than reading as part of any one inspector (Clip/Layer/
        // Master). Built AFTER the columns for z-order — see the build call near
        // the end of this fn. height() is a pure getter, safe to read here.
        let macros_h = self.macros_panel.height();
        let macros_y = rect.y;

        // One content box shared by the macros strip, the tab strip, AND the
        // section cards, so all three line up on the same left and right inset
        // (the tabs used to span the full width and visibly stick out past the
        // cards). `content_w` reserves the scrollbar gutter on the right; the
        // cards' right edge lands at `col_x + content_w`, so tabs/macros sized to
        // it match exactly.
        let col_x = rect.x + COLUMN_PAD;
        let content_w = (rect.width - COLUMN_PAD * 2.0 - SCROLLBAR_W).max(0.0);
        let full_col_w = (rect.width - COLUMN_PAD * 2.0).max(0.0);

        // Tab strip below the macros: the rungs of the current selection
        // (Clip · Layer · Group · Master), active one highlighted. Inset to the
        // shared content box.
        let tab_h = TAB_STRIP_HEIGHT;
        let tab_y = macros_y + macros_h + 2.0; // 2px gap below macros
        self.build_tab_strip(tree, Rect::new(col_x, tab_y, content_w, tab_h));
        let (master_col_w, layer_col_w) = if self.master_visible() {
            (full_col_w, 0.0)
        } else {
            (0.0, full_col_w)
        };
        // Aliases so the per-section build blocks below read unchanged.
        let left_x = col_x;
        let right_x = col_x;
        let left_content_w = if self.master_visible() { content_w } else { 0.0 };
        let right_content_w = if self.master_visible() { 0.0 } else { content_w };
        self.column_split_x = if self.master_visible() {
            rect.x + rect.width
        } else {
            rect.x
        };

        // Columns start below the tab strip.
        let columns_y = tab_y + tab_h + 2.0; // 2px gap below tabs
        let columns_h = (rect.y + rect.height - columns_y).max(0.0);
        self.columns_y = columns_y;
        self.columns_height = columns_h;

        // `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 stopgap removal: this used to
        // be a bespoke, root-parented `ClipRegion` (`content_clip_id`) — the
        // BUG-060 hand-clip the design forbids by name — covering
        // `(rect.x, columns_y, rect.width, columns_h)` so no card content
        // could paint past the inspector's own bottom edge. It's gone: the
        // outer `begin_region` the inspector now builds under (`ui_root.rs`)
        // clips the WHOLE inspector rect unconditionally, so that job is
        // structural now, not this panel's to do by hand.
        //
        // It was NOT, however, doing nothing — decided empirically, not by
        // reading (`inspector.rs`'s
        // `content_clip_prevents_scrolled_columns_painting_over_the_tab_strip`
        // test forced a scroll far enough to push several cards' raw bounds
        // above `columns_y`, into the pinned tab-strip's territory, with
        // this clip's `CLIPS_CHILDREN` flag removed as a controlled
        // experiment — and the GPU-cull replica still reported zero pixels
        // reaching the tab strip). The reason: `master_scroll`/`layer_scroll`
        // (`ScrollContainer::begin`, just below) each mint their OWN
        // `CLIPS_CHILDREN` clip at the SAME `columns_y` top edge — this
        // node's Y-range was always a strict subset of whichever column is
        // active. Deleted, not kept-and-reparented: proven redundant, not
        // merely unproven-necessary.

        // ── MASTER COLUMN (full width when active, else collapsed) ──
        let left_clip_rect = Rect::new(left_x, columns_y, master_col_w, columns_h);
        self.master_scroll.begin(tree, left_clip_rect);
        let left_start = tree.count();

        {
            let mut cy = self.master_scroll.content_y(0.0);
            if self.master_visible() {
                let section_h = self.master_column_height();
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(left_x, cy, left_content_w, section_h),
                );
                cy += SECTION_CARD_PAD;

                let inner_x = left_x + SECTION_INSET;
                let inner_w = left_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.master_chrome.compute_height();
                self.master_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.master_chrome.is_collapsed() {
                    for card in &mut self.effects[Self::SCOPE_MASTER] {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_master_effect_btn = chrome::materialize(
                        tree,
                        &add_effect_button_view(),
                        Rect::new(inner_x, cy, inner_w, ADD_EFFECT_BTN_H),
                    )
                    .into_iter()
                    .find(|(k, _)| *k == KEY_ADD_EFFECT_BTN)
                    .map(|(_, id)| id);
                    cy += ADD_EFFECT_BTN_H + SECTION_GAP;
                }
            }
            // D17 "delete collapse" — dying cards render below everything
            // else in this column (append-only, see `master_dying`'s doc
            // comment), still shrinking toward zero height until their
            // exit-state finishes. Recomputes the same inset `inner_x`/
            // `inner_w` the live-card loop above used (out of scope here —
            // that `let` lives inside the `master_visible()` block).
            if !self.master_dying.is_empty() {
                let inner_x = left_x + SECTION_INSET;
                let inner_w = left_content_w - SECTION_INSET * 2.0;
                for card in &mut self.master_dying {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
            }
        }
        self.master_scroll.reparent_content(tree, left_start);
        self.master_scroll
            .build_scrollbar(tree, left_x + left_content_w, &SCROLLBAR_STYLE);

        // ── LAYER/GROUP/CLIP COLUMN (full width when active, else collapsed) ──
        let right_clip_rect = Rect::new(right_x, columns_y, layer_col_w, columns_h);
        self.layer_scroll.begin(tree, right_clip_rect);
        let right_start = tree.count();

        {
            let mut cy = self.layer_scroll.content_y(0.0);

            // Layer section — includes gen params above layer effects
            if self.layer_visible() {
                let section_h = self.layer_column_height();
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(right_x, cy, right_content_w, section_h),
                );
                cy += SECTION_CARD_PAD;

                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.layer_chrome.compute_height();
                self.layer_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.layer_chrome.is_collapsed() {
                    // AUDIO TRIGGERS (P3b) — pinned at the top of the layer's
                    // detail content, above gen params and layer effects.
                    let at_h = self.audio_trigger_section.height();
                    self.audio_trigger_section
                        .build(tree, Rect::new(inner_x, cy, inner_w, at_h));
                    cy += at_h + SECTION_GAP;

                    if let Some(ref mut gp) = self.gen_params {
                        let gp_h = gp.compute_height();
                        gp.build(tree, Rect::new(inner_x, cy, inner_w, gp_h));
                        cy += gp_h + SECTION_GAP;
                    }

                    for card in &mut self.effects[Self::SCOPE_LAYER] {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_layer_effect_btn = chrome::materialize(
                        tree,
                        &add_effect_button_view(),
                        Rect::new(inner_x, cy, inner_w, ADD_EFFECT_BTN_H),
                    )
                    .into_iter()
                    .find(|(k, _)| *k == KEY_ADD_EFFECT_BTN)
                    .map(|(_, id)| id);
                    cy += ADD_EFFECT_BTN_H + SECTION_GAP;
                }
            }
            // D17 "delete collapse" — see the matching comment in the master
            // column above. `inner_x`/`inner_w` recomputed the same way (out
            // of scope here, inside `layer_visible()`'s block).
            if !self.layer_dying.is_empty() {
                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;
                for card in &mut self.layer_dying {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
            }

            // Clip section — its own card below the layer section, shown when a
            // clip is selected. Holds the per-clip chrome (BPM / warp / loop).
            if self.clip_visible() && self.clip_chrome.has_clip() {
                let clip_top = self.layer_scroll.content_y(0.0) + self.layer_column_height();
                let section_h =
                    SECTION_CARD_PAD + self.clip_chrome.compute_height() + SECTION_CARD_PAD;
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(right_x, clip_top, right_content_w, section_h),
                );
                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;
                let chrome_h = self.clip_chrome.compute_height();
                self.clip_chrome.build(
                    tree,
                    Rect::new(inner_x, clip_top + SECTION_CARD_PAD, inner_w, chrome_h),
                );
            }
            // No `else` to clear the clip range: the up-front reset already left it
            // empty, so an un-built clip section reports not-live without a second
            // bookkeeping site.
        }
        self.layer_scroll.reparent_content(tree, right_start);
        self.layer_scroll
            .build_scrollbar(tree, right_x + right_content_w, &SCROLLBAR_STYLE);

        // Both columns' scroll clips (`ScrollContainer::begin` always mints
        // its clip node with `parent: None`) are still tree roots here — no
        // longer swept under a bespoke `content_clip_id` (removed, see the
        // comment above `columns_y`'s scroll containers begin). The caller's
        // `begin_region`/`end_region` wrap (`ui_root.rs`) sweeps them
        // directly, same as every other `None`-rooted node this panel
        // builds (D1/D4).

        // ── MACROS STRIP (pinned to the top, above the tab strip; built last so
        // it draws on top of any column content) ──
        let macros_rect = Rect::new(left_x, macros_y, content_w, macros_h);
        self.macros_panel.build(tree, macros_rect);

        self.update_scroll_bounds();
        self.update_scrollbar(tree);

        self.cache_node_count = tree.count() - self.cache_first_node;
    }
}
