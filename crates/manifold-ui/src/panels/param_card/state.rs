//! Standalone aux types + tween/collapse/drawer animation state for
//! [`ParamCardPanel`] (P-S4 split of `param_card.rs`).

use super::*;

/// Which kind of preset a card is displaying. Carries the small set of real
/// behavioral differences between the effect and generator inspector cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamCardKind {
    Effect,
    Generator,
}

/// Where the card is being shown — which decides its chrome, not its data.
///
/// `Perform` is the inspector / live surface: the full performing card with its
/// drag-reorder handle, the "open graph editor" cog, and the right-click
/// perform-mapping menu. `Author` is the graph editor's left lane: the same
/// instrument, but the perform-only chrome is suppressed (you're already in the
/// editor, reorder is meaningless against one card, and the perform-mapping menu
/// is replaced by the sideways mapping drawer) and each mappable row gains a
/// chevron at its right edge that opens that drawer. Default is `Perform` so
/// every existing inspector card is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardContext {
    Perform,
    Author,
}

/// A generator string parameter — rendered as a clickable text-field row
/// below the slider rows. Generator-only; effects carry an empty list.
#[derive(Debug, Clone)]
pub struct ParamCardStringInfo {
    pub name: String,
    pub key: String,
    pub value: String,
    /// If true, clicking this param opens a dropdown instead of text input.
    pub use_dropdown: bool,
}

/// Config for the "3D Shading" toggle + D3 knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
/// P5b) — the union `ParamSurface` carries for both effect and generator
/// cards. Always present (mirrors `PresetInstance.relight`/`relight_params`
/// always being live on the instance): the card renders the six knobs +
/// Height From row greyed rather than hidden when `enabled` is false
/// (no-conditionally-visible-ui), so the values must survive a
/// toggle-off/toggle-on round trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelightCardConfig {
    pub enabled: bool,
    pub light_x: f32,
    pub light_y: f32,
    pub relief: f32,
    pub ao_intensity: f32,
    pub shadow_softness: f32,
    pub gain: f32,
    pub height_from: UiRelightHeightFrom,
}

/// One D3 knob's static shape — label + clamp range + reset default. The
/// SINGLE source both `build_relight_rows` (rendering) and the drag hit-test
/// in `handle_pointer_down` read, so the two can't drift out of range.
pub(crate) struct RelightFieldSpec {
    pub(crate) field: UiRelightField,
    pub(crate) label: &'static str,
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) default: f32,
}

/// D3's proven ranges, in `RelightField` declaration order — mirror the
/// underlying atoms' own `ParamDef` ranges (`lambert_directional`/
/// `heightfield_shadow`'s light_x/y, `ssao_gtao`'s relief/intensity,
/// `heightfield_shadow`'s softness, `node.gain`'s gain). `ui` cannot read the
/// registry directly (`RelightCardConfig`'s doc), so these are pinned here.
pub(crate) const RELIGHT_FIELD_SPECS: [RelightFieldSpec; 6] = [
    RelightFieldSpec { field: UiRelightField::LightX, label: "Light X", min: -1.0, max: 1.0, default: 0.4 },
    RelightFieldSpec { field: UiRelightField::LightY, label: "Light Y", min: -1.0, max: 1.0, default: 0.6 },
    RelightFieldSpec { field: UiRelightField::Relief, label: "Relief", min: 0.01, max: 2.0, default: 0.25 },
    RelightFieldSpec {
        field: UiRelightField::AoIntensity,
        label: "AO Intensity",
        min: 0.0,
        max: 4.0,
        default: 1.3,
    },
    RelightFieldSpec {
        field: UiRelightField::ShadowSoftness,
        label: "Shadow Softness",
        min: 0.0,
        max: 1.0,
        default: 0.5,
    },
    RelightFieldSpec { field: UiRelightField::Gain, label: "Gain", min: 0.0, max: 4.0, default: 1.4 },
];

impl RelightCardConfig {
    /// Read one knob's current value by field — the single accessor the
    /// row-drag path uses so it never has to match on `RelightField` itself.
    pub(crate) fn value(&self, field: UiRelightField) -> f32 {
        match field {
            UiRelightField::LightX => self.light_x,
            UiRelightField::LightY => self.light_y,
            UiRelightField::Relief => self.relief,
            UiRelightField::AoIntensity => self.ao_intensity,
            UiRelightField::ShadowSoftness => self.shadow_softness,
            UiRelightField::Gain => self.gain,
        }
    }

    /// Live-preview write for a mid-drag update (never committed here — the
    /// app layer owns the undo-tracked commit on release, mirroring every
    /// other slider row).
    pub(crate) fn set_value(&mut self, field: UiRelightField, value: f32) {
        match field {
            UiRelightField::LightX => self.light_x = value,
            UiRelightField::LightY => self.light_y = value,
            UiRelightField::Relief => self.relief = value,
            UiRelightField::AoIntensity => self.ao_intensity = value,
            UiRelightField::ShadowSoftness => self.shadow_softness = value,
            UiRelightField::Gain => self.gain = value,
        }
    }
}

impl Default for RelightCardConfig {
    /// D3's proven v6 recipe defaults — mirrors
    /// `manifold_core::effects::RelightParams::default()` field-for-field.
    /// `ui` cannot depend on `manifold-core`; kept in sync by
    /// `preset_to_config`'s (manifold-app) doc comment pointing back here.
    fn default() -> Self {
        Self {
            enabled: false,
            light_x: 0.4,
            light_y: 0.6,
            relief: 0.25,
            ao_intensity: 1.3,
            shadow_softness: 0.5,
            gain: 1.4,
            height_from: UiRelightHeightFrom::Auto,
        }
    }
}

/// One param row's driver/envelope/automation modulation facts — carried as
/// `ParamRow::modulation`. Collapses the former fourteen parallel per-param
/// vecs (D3, `docs/WIDGET_TREE_DESIGN.md` P1a) into one struct per row.
#[derive(Debug, Clone)]
pub struct RowMod {
    /// Driver exists and is enabled.
    pub driver_active: bool,
    /// Envelope exists and is enabled.
    pub envelope_active: bool,
    /// Driver trim min (normalized). Defaults to 0.0.
    pub trim_min: f32,
    /// Driver trim max (normalized). Defaults to 1.0.
    pub trim_max: f32,
    /// Envelope target (the orange handle, normalized). Default 1.0.
    pub target_norm: f32,
    /// Envelope decay time in beats. Default 1.0.
    pub env_decay: f32,
    /// Driver beat division button index (0-10). -1 if no driver.
    pub driver_beat_div_idx: i32,
    /// Driver waveform index (0-4). -1 if no driver.
    pub driver_waveform_idx: i32,
    /// Driver reversed state.
    pub driver_reversed: bool,
    /// Driver dotted modifier active.
    pub driver_dotted: bool,
    /// Driver triplet modifier active.
    pub driver_triplet: bool,
    /// Driver free-running period in beats (`Some` => free mode).
    pub driver_free_period: Option<f32>,
    /// An enabled automation lane (≥1 point) exists on this instance for
    /// this param — drives the red "automated" dot (P4 §7).
    pub automation_active: bool,
    /// That lane is currently overridden (latched) — the dot grays instead
    /// of showing red.
    pub automation_overridden: bool,
}

impl Default for RowMod {
    fn default() -> Self {
        Self {
            driver_active: false,
            envelope_active: false,
            trim_min: 0.0,
            trim_max: 1.0,
            target_norm: 1.0,
            env_decay: 1.0,
            driver_beat_div_idx: -1,
            driver_waveform_idx: -1,
            driver_reversed: false,
            driver_dotted: false,
            driver_triplet: false,
            driver_free_period: None,
            automation_active: false,
            automation_overridden: false,
        }
    }
}

// ── ParamCardState ───────────────────────────────────────────────

/// Presenter-owned visual state for one parameter card — the single source of
/// truth for all data-derived visuals (badges + per-param modulation). Unifies
/// the former `EffectCardState` / `GenParamState`. The badge aggregates only
/// drive the effect-card header chips; generators leave them `false`.
pub struct ParamCardState {
    /// Aggregate: any param has an active driver (DRV badge).
    pub has_drv: bool,
    /// Aggregate: any param has an active envelope (ENV badge).
    pub has_env: bool,
    /// Aggregate: any param has an Ableton mapping (ABL badge).
    pub has_abl: bool,
    /// Aggregate: any param has an armed audio modulation (AUD badge).
    pub has_audio: bool,
    /// The card's graph diverges from the catalog default (MOD badge only).
    pub has_graph_mod: bool,
    /// Shared per-param modulation state (driver/envelope expansion, trim,
    /// target, ADSR, driver config).
    pub mod_state: ParamModState,
}

impl ParamCardState {
    pub fn new(param_count: usize) -> Self {
        Self {
            has_drv: false,
            has_env: false,
            has_abl: false,
            has_audio: false,
            has_graph_mod: false,
            mod_state: ParamModState::allocate(param_count),
        }
    }
}


impl ParamCardPanel {
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    /// Whether the D17 "spawn pop" scale-in is still in flight. Exposed for
    /// `InspectorCompositePanel`'s `reconcile_cards` tests (in a different
    /// module — `spawn_scale` itself stays private).
    pub fn is_spawning(&self) -> bool {
        self.spawn_scale.is_animating()
    }

    /// Whether the D17 "card collapse" tween is still in flight. Same
    /// cross-module test-accessor purpose as `is_spawning`.
    pub fn is_collapse_animating(&self) -> bool {
        self.collapse_anim.is_animating()
    }

    /// Force the collapsed flag directly, snapping `collapse_anim` (no ease).
    /// This is the "test/automation harness drives it directly" setter
    /// (mirrors `chevron_node_id`'s doc comment) — production code toggles
    /// collapse through the model (`fx.collapsed` /
    /// `PanelAction::EffectCollapseToggle`) and `configure()`'s
    /// `sync_collapse_anim`, which eases. The one production caller of this
    /// setter is the generator param panel (`ui_bridge/inspector.rs`), whose
    /// `build_generator` can't render a partial-height body anyway (see
    /// `collapse_anim`'s doc comment) — always-snap is correct for it too.
    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.is_collapsed = collapsed;
        let target = if collapsed { 0.0 } else { 1.0 };
        self.collapse_anim.snap(target);
        self.collapse_configured = true;
    }

    /// Point `collapse_anim` at the target implied by `is_collapsed`/`kind`.
    /// Called from `configure()` — the real per-rebuild path a model-driven
    /// collapse toggle (`PanelAction::EffectCollapseToggle`) round-trips
    /// through. Effect cards ease once already configured once (mirrors
    /// `drawer_height_anim`'s "don't slide in on first appearance" rule,
    /// and — since `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 (D4) —
    /// identically in both contexts, now that the editor ticks its
    /// inspector every frame); every other case (first-ever configure, or a
    /// Generator card whose `build_generator` can't render a partial-height
    /// body) snaps instantly so `compute_height`/`build` never disagree.
    pub(crate) fn sync_collapse_anim(&mut self) {
        let target = if self.is_collapsed { 0.0 } else { 1.0 };
        let eases = self.kind == ParamCardKind::Effect && self.collapse_configured;
        if eases {
            self.collapse_anim.set_target(target);
        } else {
            self.collapse_anim.snap(target);
        }
        self.collapse_configured = true;
    }

    /// D17 "card collapse" fraction: `1.0` fully expanded, `0.0` fully
    /// collapsed, eased between by `collapse_anim` for Effect cards.
    /// Generator cards always read the settled boolean (see `collapse_anim`'s
    /// doc comment) — `sync_collapse_anim` snaps them, so this is exactly
    /// `0.0`/`1.0` there too, just never mid-flight.
    pub(crate) fn collapse_frac(&self) -> f32 {
        self.collapse_anim.value().clamp(0.0, 1.0)
    }

    /// P2 "caret rotate" — maps `collapse_frac()` (reusing `collapse_anim`,
    /// NOT a second animation clock) onto the down-pointing chevron glyph's
    /// rotation: expanded (`frac == 1.0`) sits at 0° (▼), collapsed
    /// (`frac == 0.0`) rotates to -90° (▶, "closing"). Applied via
    /// `UIStyle.transform` (`docs/UI_TRANSFORM_STACK_DESIGN.md`), which
    /// pivots about the chevron node's own rect center — no manual pivot
    /// math here, no glyph swap.
    pub(crate) fn chevron_angle(&self) -> f32 {
        (self.collapse_frac() - 1.0) * std::f32::consts::FRAC_PI_2
    }

    /// D17 "spawn pop" — call once, right after the first `configure()` on a
    /// truly new panel (`InspectorCompositePanel::reconcile_cards`, the only
    /// caller). Restarts `spawn_scale` from 0.94 easing to 1.0 with the
    /// magnetic-snap back-out curve (D15's `Curve::Snap`).
    pub fn fire_spawn_pop(&mut self) {
        self.spawn_scale = AnimF32::new(0.94, color::MOTION_MED_MS).with_curve(crate::anim::Curve::Snap);
        self.spawn_scale.set_target(1.0);
    }

    /// D17 "delete collapse" (exit-state pattern) — call once when this card
    /// has just been dropped from the model's effect list
    /// (`InspectorCompositePanel::reconcile_cards` moves it into a
    /// panel-owned `dying` list instead of discarding it here). Retargets the
    /// existing card-collapse mechanism to fully collapsed (reflows whatever
    /// follows it, exactly like a user-triggered collapse) and starts the
    /// exit fade timer `is_delete_finished` reads.
    pub fn begin_delete_collapse(&mut self) {
        self.collapse_anim.set_target(0.0);
        let mut fade = Transient::default();
        fade.fire(color::MOTION_MED_MS);
        self.delete_fade = Some(fade);
    }

    /// Whether this dying card's exit animation has fully played out — both
    /// the fade timer AND the height collapse have settled. The caller
    /// (`InspectorCompositePanel`'s `dying` list) drops the panel for good
    /// once this is `true`; until then it keeps calling `tick_drawers`/
    /// `build` on it every frame, same as any live card.
    pub fn is_delete_finished(&self) -> bool {
        self.delete_fade
            .as_ref()
            .is_some_and(|f| f.progress().is_none())
            && !self.collapse_anim.is_animating()
    }

    /// P2 "value snap-back" (D15): meant to be called by the app-side dispatch
    /// the instant the RIGHT-CLICK reset-to-default gesture commits — the
    /// model has ALREADY snapped to `to` (raw param value) by the time this
    /// runs; this only retargets the row's `value_snapback` `AnimF32`
    /// (Curve::Snap) so the slider FILL eases from `from` to `to` instead of
    /// jumping — the data is never delayed behind this. BUG-061 folded the
    /// param right-click reset into the generic `SliderReset` trio (plain
    /// `ParamChanged`, no easing) — no production call site remains; kept
    /// for its own unit tests below. A no-op if `param_id` isn't one of this
    /// card's rows (stale/mismatched target).
    pub fn begin_value_snapback(&mut self, param_id: &manifold_foundation::ParamId, from: f32, to: f32) {
        let Some(pi) = self.rows.iter().position(|p| &p.id == param_id) else {
            return;
        };
        let Some(info) = self.rows.get(pi) else {
            return;
        };
        let from_norm = BitmapSlider::value_to_normalized(from, info.spec.min, info.spec.max);
        let to_norm = BitmapSlider::value_to_normalized(to, info.spec.min, info.spec.max);
        if let Some(anim) = self.value_snapback.get_mut(pi) {
            anim.snap(from_norm);
            anim.set_target(to_norm);
        }
    }

    /// Height contributed by the modulation config drawer for one slider param.
    /// Mirrors `build_param_row` exactly: 0 configs → 0; 1 → that config's
    /// height; ≥2 → the tab strip plus the single shown config (they no longer
    /// stack). Track overlays (trim bars, envelope target) add no height.
    /// Compact mode hides every drawer, so the contribution is 0.
    pub(crate) fn row_drawer_height(&self, i: usize) -> f32 {
        if self.compact {
            return 0.0;
        }
        let Some(info) = self.rows.get(i) else {
            return 0.0;
        };
        let active = active_mod_tabs(&self.state.mod_state, info, i);
        let h = match active.len() {
            0 => return 0.0,
            1 => mod_config_height(active[0], info, &self.state.mod_state, i),
            _ => {
                let stored = self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver);
                let shown = resolve_active_tab(&active, stored).unwrap_or(active[0]);
                MOD_TAB_STRIP_H + mod_config_height(shown, info, &self.state.mod_state, i)
            }
        };
        // Match the build's post-drawer break (see `build_param_row`).
        h + DRAWER_BOTTOM_GAP
    }

    /// Reserved drawer height for row `i`, following the P1 open/close tween while
    /// one is in flight. Equals `row_drawer_height(i)` once settled (and always,
    /// for a card the inspector doesn't tick), so `compute_height` and the build
    /// agree and a settled card lays out exactly as before the motion work.
    pub(crate) fn animated_drawer_height(&self, i: usize) -> f32 {
        match self.drawer_height_anim.get(i) {
            // Only override while a tween is actually in flight. Once settled,
            // `row_drawer_height` is the live source of truth — so a state change
            // that doesn't route through `configure` (e.g. a direct test mutation,
            // or any future in-place edit) is reflected immediately, and build
            // (which also only supplies a reveal while `is_animating`) stays in
            // exact agreement with this reserved height.
            Some(a) if a.is_animating() => a.value(),
            _ => self.row_drawer_height(i),
        }
    }

    /// Force every per-card tween (drawer height, tab-ink slide, collapse,
    /// spawn pop, delete fade, value flash, value snap-back) to its settled
    /// end state in one call (BUG-073 fix shape (b)): a headless `--script`
    /// driver has no per-frame timer, so a tween armed mid-script — e.g. a
    /// newly-armed drawer growing a card's row count — would otherwise never
    /// advance past its t=0 state unless the script happens to insert a
    /// `Step` afterward. Reuses `tick_drawers`/`tick_value_flash`'s own tick
    /// logic with a `dt_ms` large enough that every tween's `t` clamps to 1.0
    /// in one call, rather than duplicating the settle math per-field.
    /// Returns whether anything was actually mid-flight — the caller only
    /// needs to force a rebuild when this is `true`.
    pub fn skip_to_settled(&mut self, tree: &mut UITree) -> bool {
        let was_animating = self.collapse_anim.is_animating()
            || self.spawn_scale.is_animating()
            || self.delete_fade.as_ref().is_some_and(|f| f.progress().is_some())
            || self.drawer_height_anim.iter().any(|a| a.is_animating())
            || self.mod_tab_ink.iter().any(|a| a.is_animating())
            || self.value_flash.iter().any(|f| f.progress().is_some())
            || self.value_snapback.iter().any(|a| a.is_animating());
        if was_animating {
            const HUGE_DT_MS: f32 = 1.0e9;
            self.tick_drawers(HUGE_DT_MS);
            self.tick_value_flash(tree, HUGE_DT_MS);
        }
        was_animating
    }

    /// Advance this card's drawer-height tweens by `dt_ms`; returns true while any
    /// is still in flight. Called by the inspector's per-frame `update()`; the
    /// value it advances is read by the *next* `build()` (which the app's
    /// `drawer_anim_active` poll forces while this returns true).
    pub fn tick_drawers(&mut self, dt_ms: f32) -> bool {
        let mut any = false;
        // P2 card collapse + spawn pop ride the same per-frame rail.
        any |= self.collapse_anim.tick(dt_ms);
        any |= self.spawn_scale.tick(dt_ms);
        if let Some(fade) = self.delete_fade.as_mut() {
            any |= fade.tick(dt_ms);
        }
        for a in &mut self.drawer_height_anim {
            any |= a.tick(dt_ms);
        }
        // D1 tab-ink slide rides the same per-frame rail — one bool bubble-up,
        // no second app-side poll.
        for a in &mut self.mod_tab_ink {
            any |= a.tick(dt_ms);
        }
        any
    }

    /// P2 value-change flash + value snap-back: advance every param's
    /// one-shot `Transient` and paint the value-text color accordingly — an
    /// accent while `progress()` is `Some`, reverted to the normal slider
    /// text color the instant it finishes. A plain style write to an
    /// already-built node (no layout change), so unlike `tick_drawers` this
    /// never needs the app's forced-rebuild poll; it just needs to run every
    /// frame, which it already does from the same
    /// `InspectorCompositePanel::update()` call site.
    ///
    /// Also drives P2 "value snap-back" (D15, `value_snapback`/
    /// `begin_value_snapback`): `sync_values`'s dirty-check only calls
    /// `BitmapSlider::update_value` the ONE frame the model value actually
    /// changes, so a settling fill needs its own per-frame repaint here for
    /// every frame after that — `tick`ing `value_snapback[i]` and
    /// re-positioning just the fill/thumb (never the text, which is already
    /// correct — the data snapped instantly) at the eased normalized value.
    pub fn tick_value_flash(&mut self, tree: &mut UITree, dt_ms: f32) -> bool {
        let mut any = false;
        for (i, flash) in self.value_flash.iter_mut().enumerate() {
            let was_active = flash.progress().is_some();
            let still_active = flash.tick(dt_ms);
            any |= still_active;
            if !still_active && !was_active {
                continue; // idle both before and after — nothing to repaint
            }
            let Some(ref ids) = self.row_host.slider_ids[i] else {
                continue;
            };
            // Read-modify-write on the node's existing style so bg/radius/font
            // (which differ between the effect and generator sliders) are
            // never guessed at here — only `text_color` changes.
            let Some(mut style) = tree.get_node(ids.value_text).map(|n| n.style) else {
                continue;
            };
            style.text_color = if still_active {
                color::ACCENT_BLUE_C32
            } else {
                // Just finished this tick — revert once, not every frame after.
                color::SLIDER_TEXT_C32
            };
            tree.set_style(ids.value_text, style);
        }
        for (i, anim) in self.value_snapback.iter_mut().enumerate() {
            if !anim.is_animating() {
                continue;
            }
            any |= anim.tick(dt_ms);
            let Some(ref ids) = self.row_host.slider_ids[i] else {
                continue;
            };
            // Re-derive the (already-settled) value text rather than touch
            // it — only the fill/thumb position eases; `update_value` writes
            // all three, so pass the unchanged text back through unmodified.
            let Some(text) = tree.get_node(ids.value_text).map(|n| n.text.clone().unwrap_or_default())
            else {
                continue;
            };
            BitmapSlider::update_value(tree, ids, anim.value(), &text);
        }
        any
    }

    /// D1 "tab-ink slide": after a row's mod-config tab strip is built, point
    /// this param's ink tween at the shown tab's on-screen x and draw the
    /// sliding underline. A no-op when fewer than two configs are active (no
    /// strip was built — `self.row_host.mod_tab_ids[i]` is empty).
    pub(crate) fn sync_mod_tab_ink(&mut self, tree: &mut UITree, i: usize) {
        let tabs = self.row_host.mod_tab_ids[i].clone();
        if tabs.len() < 2 {
            if let Some(ink) = self.mod_tab_ink.get_mut(i) {
                ink.snap(0.0);
            }
            return;
        }
        let shown = resolve_active_tab(
            &tabs.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
            self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
        );
        let Some((id, tab)) = shown.and_then(|s| tabs.iter().find(|(_, t)| *t == s).copied())
        else {
            return;
        };
        let rect = tree.get_bounds(id);
        let Some(ink) = self.mod_tab_ink.get_mut(i) else {
            return;
        };
        if ink.target() == 0.0 && ink.value() == 0.0 {
            ink.snap(rect.x);
        } else {
            ink.set_target(rect.x);
        }
        let ink_y = rect.y + rect.height - MOD_TAB_INK_H;
        tree.add_panel(
            Some(id),
            ink.value(),
            ink_y,
            rect.width,
            MOD_TAB_INK_H,
            UIStyle {
                bg_color: mod_tab_accent(tab),
                ..UIStyle::default()
            },
        );
    }
}
