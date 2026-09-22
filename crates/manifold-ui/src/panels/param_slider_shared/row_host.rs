//! `RowHost` — the shared per-row id-bundle machinery + row-index + row-action
//! routing lifted out of `ParamCardPanel` (P-S2, UI funnel decomposition).
//!
//! A parameter card (the effect/generator inspector card, and — after P-S3 —
//! the scene properties card) renders a column of parameter rows and must map
//! a clicked/pressed `NodeId` back to the row + role that owns it. That map,
//! the per-row node-id bundles it's built from, and the routing that turns a
//! resolved `(row, role)` into a `PanelAction` are identical across every
//! card kind. `RowHost` is the single home for that machinery so a scene card
//! can BE one instead of hand-copying it (the `SceneCardState` twin P-S3
//! deletes).
//!
//! `RowHost` owns registration, routing and the active row gesture. The data a card
//! renders and edits — the `ParamRow`s, the `ParamModState`, the value /
//! osc / tab caches — stays on the owning panel and is passed to the routing
//! methods by reference. That keeps the seam tight: `RowHost` is the widget-id
//! bookkeeping and interaction logic; the panel keeps its model and layout.

use super::*;
use crate::panels::copy_to_clipboard_label::CopyToClipboardLabelState;
use crate::param_surface::{RowIndex, RowRole, RowSpec};
use crate::panels::scrub::ScrubGesture;
use crate::slider::TrackSpan;
use crate::{ModulationAction, ParamsAction};

/// The mutable inputs owned by a row-rendering panel.  RowHost only retains
/// the stable address and gesture payload; all current rows and display state
/// remain in the caller so rebuilds can safely reorder them.
pub(crate) struct RowInteraction<'a> {
    pub target: &'a GraphParamTarget,
    pub rows: &'a mut [ParamRow],
    pub modulation: &'a mut ParamModState,
    pub values: &'a mut [f32],
    pub row_indices: &'a ahash::AHashMap<String, usize>,
}

#[derive(Debug, Clone, Copy)]
enum RowDragKind {
    Param,
    Trim { kind: TrimKind, is_min: bool },
    EnvelopeTarget,
    EnvelopeDecay,
    AudioShape(AudioShapeParam),
    AudioStepAmount,
}

#[derive(Debug, Clone)]
struct RowDrag {
    kind: RowDragKind,
    param_id: manifold_foundation::ParamId,
    start_value: f32,
    range: (f32, f32),
    last_value: f32,
    last_range: (f32, f32),
    last_pos: Vec2,
}

fn set_audio_shape_value(state: &mut ParamModState, row: usize, which: AudioShapeParam, value: f32) {
    match which {
        AudioShapeParam::Sensitivity => {
            if let Some(slot) = state.audio_rows.get_mut(row) { slot.sensitivity = value; }
        }
        AudioShapeParam::Attack => {
            if let Some(slot) = state.audio_rows.get_mut(row) { slot.attack_ms = value; }
        }
        AudioShapeParam::Release => {
            if let Some(slot) = state.audio_rows.get_mut(row) { slot.release_ms = value; }
        }
    }
}

fn audio_shape_index(which: AudioShapeParam) -> usize {
    match which {
        AudioShapeParam::Sensitivity => 0,
        AudioShapeParam::Attack => 1,
        AudioShapeParam::Release => 2,
    }
}

fn audio_shape_max(which: AudioShapeParam) -> f32 {
    match which {
        AudioShapeParam::Sensitivity => AUDIO_SENS_MAX,
        AudioShapeParam::Attack => AUDIO_ATTACK_MAX_MS,
        AudioShapeParam::Release => AUDIO_RELEASE_MAX_MS,
    }
}

fn current_trim_range(ctx: &RowInteraction<'_>, row: usize, kind: TrimKind) -> Option<(f32, f32)> {
    match kind {
        TrimKind::Driver => Some((
            ctx.modulation.trim_min.get(row).copied().unwrap_or(0.0),
            ctx.modulation.trim_max.get(row).copied().unwrap_or(1.0),
        )),
        TrimKind::Ableton => ctx.rows.get(row).and_then(|r| r.mapping.ableton_range),
        TrimKind::Audio => Some((
            ctx.modulation.audio_rows.get(row).map(|r| r.range_min).unwrap_or(0.0),
            ctx.modulation.audio_rows.get(row).map(|r| r.range_max).unwrap_or(1.0),
        )),
    }
}

fn captured_target_matches(address: &ValueRef, target: &GraphParamTarget) -> bool {
    match address {
        ValueRef::Param(captured, _)
        | ValueRef::Trim(_, captured, _)
        | ValueRef::EnvelopeTarget(captured, _)
        | ValueRef::EnvDecay(captured, _)
        | ValueRef::EnvelopeStepAmount(captured, _)
        | ValueRef::AudioModShape(captured, _, _)
        | ValueRef::AudioModStepAmount(captured, _) => captured == target,
        _ => true,
    }
}

/// Release-mode once-per-id loud signal for the id-join miss invariant (INV-6):
/// a built row whose id has NO live manifest entry this frame. `debug_assert!`
/// already panics in dev; this keeps the release path from freezing a row
/// silently. Reachable only if a manifest mutation skipped the structural
/// reconfigure that rebuilds the rows — an upstream bug, surfaced here rather
/// than swallowed.
pub(crate) fn warn_join_gap_once(id: &str) {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(id.to_string()) {
        eprintln!(
            "param value sync: built row id {id:?} has no live manifest entry — a manifest \
             mutation skipped its structural reconfigure; row frozen this frame (INV-6)"
        );
    }
}

/// Per-row widget-id bundles + the reverse `WidgetId → (row, role)` index for
/// one parameter card. Every field is row-parallel (indexed by param row),
/// except `row_index` (the flat reverse map) and `section_header_ids` (rebuilt
/// each build pass). Populated by the card's row builders as each row's
/// controls land; consumed by [`RowHost::reindex_row`] /
/// [`RowHost::register_intents`] / [`RowHost::row_action`].
pub(crate) struct RowHost {
    pub(crate) slider_ids: Vec<Option<SliderNodeIds>>,
    /// Per-param right-click reset for `slider_ids[pi]`'s track — a parallel
    /// array (rather than folding into `slider_ids`) so the many existing
    /// `slider_ids[pi].track`/etc. access sites are untouched. `Some` exactly
    /// when `slider_ids[pi]` is `Some` (BUG-070 follow-through).
    pub(crate) slider_resets: Vec<Option<PanelAction>>,
    /// Per-param transparent full-row hit catcher behind the slider widgets.
    pub(crate) row_catcher_ids: Vec<Option<NodeId>>,
    pub(crate) driver_btn_ids: Vec<Option<NodeId>>,
    pub(crate) envelope_btn_ids: Vec<Option<NodeId>>,
    pub(crate) driver_config_ids: Vec<Option<crate::panels::drawer::DrawerIds>>,
    /// Per-param "A" audio-mod button node id.
    pub(crate) audio_btn_ids: Vec<Option<NodeId>>,
    /// Per-param explicit automation-entry button in the label cell.
    pub(crate) automation_btn_ids: Vec<Option<NodeId>>,
    /// Per-param audio drawer ids + send count (for click resolution). An
    /// `is_trigger_gate` row's "A" button + drawer live here too (section 9).
    pub(crate) audio_configs: Vec<Option<(crate::panels::drawer::DrawerIds, usize)>>,
    /// Per-param collapsed-row mode-indicator label (section 9, `is_trigger_gate`
    /// rows only).
    pub(crate) audio_trigger_mode_badge_ids: Vec<Option<NodeId>>,
    /// Per-param orange envelope target handle on the slider track (when armed).
    pub(crate) target_ids: Vec<Option<EnvelopeTargetIds>>,
    /// Per-param envelope drawer — the single "Decay" slider (when armed).
    pub(crate) envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    pub(crate) trim_ids: Vec<Option<TrimHandleIds>>,
    pub(crate) ableton_trim_ids: Vec<Option<TrimHandleIds>>,
    /// Per-param green audio-mod trim handles (when an audio mod is armed).
    pub(crate) audio_trim_ids: Vec<Option<TrimHandleIds>>,
    pub(crate) ableton_config_ids: Vec<Option<crate::panels::drawer::DrawerIds>>,
    /// Per-param modulation-config tab strip node ids paired with their
    /// `ModTab`, for routing tab clicks. Empty for rows with fewer than two
    /// active configs. Rebuilt each frame.
    pub(crate) mod_tab_ids: Vec<Vec<(NodeId, ModTab)>>,
    pub(crate) toggle_ids: Vec<Option<ToggleParamIds>>,
    /// Per-param sideways-mapping-drawer chevron (Author context, mappable rows
    /// only). `None` for rows without one.
    pub(crate) mapping_chevron_ids: Vec<Option<NodeId>>,
    /// Rebuilt every build pass: `(header_node_id, section_name)` for every
    /// section-header row drawn this frame, so a header click resolves back to
    /// its section without a second id → name map.
    pub(crate) section_header_ids: Vec<(NodeId, String)>,
    /// WidgetId → (row, role) reverse map, rebuilt every `build()` from the
    /// same rows being rendered (D5, `docs/WIDGET_TREE_DESIGN.md` P2) — the
    /// ONLY sanctioned way `handle_click`/`handle_pointer_down`/`handle_drag`
    /// identify a row element. Cleared at the top of `build()`, repopulated by
    /// `reindex_row` as each row's controls land.
    pub(crate) row_index: RowIndex,
    gesture: ScrubGesture<RowDrag>,
}

impl RowHost {
    pub(crate) fn new() -> Self {
        Self {
            slider_ids: Vec::new(),
            slider_resets: Vec::new(),
            row_catcher_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            audio_btn_ids: Vec::new(),
            automation_btn_ids: Vec::new(),
            audio_configs: Vec::new(),
            audio_trigger_mode_badge_ids: Vec::new(),
            target_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            ableton_trim_ids: Vec::new(),
            audio_trim_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            mod_tab_ids: Vec::new(),
            toggle_ids: Vec::new(),
            mapping_chevron_ids: Vec::new(),
            section_header_ids: Vec::new(),
            row_index: RowIndex::default(),
            gesture: ScrubGesture::new(),
        }
    }

    /// Resize every row-parallel bundle together.  Builders must use this
    /// entry point so a newly added [`ParamRowIds`] field cannot silently lose
    /// registration in one card kind.
    pub(crate) fn resize(&mut self, n: usize) {
        self.slider_ids.resize_with(n, || None);
        self.slider_resets.resize_with(n, || None);
        self.row_catcher_ids.resize_with(n, || None);
        self.driver_btn_ids.resize_with(n, || None);
        self.envelope_btn_ids.resize_with(n, || None);
        self.driver_config_ids.resize_with(n, || None);
        self.audio_btn_ids.resize_with(n, || None);
        self.automation_btn_ids.resize_with(n, || None);
        self.audio_configs.resize_with(n, || None);
        self.audio_trigger_mode_badge_ids.resize_with(n, || None);
        self.target_ids.resize_with(n, || None);
        self.envelope_config_ids.resize_with(n, || None);
        self.trim_ids.resize_with(n, || None);
        self.ableton_trim_ids.resize_with(n, || None);
        self.audio_trim_ids.resize_with(n, || None);
        self.ableton_config_ids.resize_with(n, || None);
        self.mod_tab_ids.resize_with(n, Vec::new);
        self.toggle_ids.resize_with(n, || None);
        self.mapping_chevron_ids.resize_with(n, || None);

    }

    /// Install all ids returned by the shared slider-row builder.  Keep this
    /// destructure exhaustive: adding a field to `ParamRowIds` must force both
    /// card consumers through this registration seam.
    pub(crate) fn install_row(
        &mut self,
        tree: &UITree,
        row: usize,
        built: crate::panels::param_slider_shared::ParamRowIds,
    ) -> f32 {
        let crate::panels::param_slider_shared::ParamRowIds {
            row_catcher,
            slider,
            slider_reset,
            trim,
            target,
            ableton_trim,
            audio_trim,
            envelope_btn,
            driver_btn,
            audio_btn,
            automation_btn,
            envelope_config,
            driver_config,
            ableton_config,
            audio_config,
            mod_tabs,
            new_cy,
        } = built;
        self.slider_ids[row] = slider;
        self.slider_resets[row] = Some(slider_reset);
        self.row_catcher_ids[row] = Some(row_catcher);
        self.trim_ids[row] = trim;
        self.target_ids[row] = target;
        self.envelope_config_ids[row] = envelope_config;
        self.ableton_trim_ids[row] = ableton_trim;
        self.audio_trim_ids[row] = audio_trim;
        self.envelope_btn_ids[row] = envelope_btn;
        self.driver_btn_ids[row] = Some(driver_btn);
        self.driver_config_ids[row] = driver_config;
        self.ableton_config_ids[row] = ableton_config;
        self.audio_btn_ids[row] = Some(audio_btn);
        self.automation_btn_ids[row] = automation_btn;
        self.audio_configs[row] = audio_config;
        self.mod_tab_ids[row] = mod_tabs;
        self.reindex_row(tree, row);
        new_cy
    }

    /// Begin a row-owned scrub from the live tree.  The captured ParamId is
    /// the only routing identity retained by the gesture; row positions are
    /// resolved again from `RowInteraction::row_indices` on every move.
    pub(crate) fn handle_pointer_down(
        &mut self,
        node: NodeId,
        pos: Vec2,
        tree: &UITree,
        ctx: RowInteraction<'_>,
    ) -> Vec<PanelAction> {
        let Some((row, role)) = self.row_index.get(tree.widget_of(node)) else {
            return Vec::new();
        };
        // A projection-disabled row takes no drag (the card's handle_click
        // drops its clicks too) — `RowSpec.disabled` is honored at dispatch,
        // never by each widget.
        if ctx.rows.get(row).is_some_and(|r| r.spec.disabled.is_some()) {
            return Vec::new();
        }
        if !matches!(role, RowRole::Slider | RowRole::EnvelopeConfig | RowRole::AudioConfig)
            || row >= ctx.rows.len()
        {
            return Vec::new();
        }
        let info = ctx.rows[row].clone();
        let target = ctx.target.clone();
        let param_id = info.id.clone();

        if role == RowRole::EnvelopeConfig {
            let Some(config) = self.envelope_config_ids.get(row).and_then(Option::as_ref) else {
                return Vec::new();
            };
            let Some(decay) = &config.decay_slider else { return Vec::new() };
            if node != decay.track { return Vec::new(); }
            let track = tree.get_bounds(decay.track);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track), pos.x).clamp(0.0, 1.0);
            let value = norm * ENV_DECAY_MAX;
            if let Some(decay_value) = ctx.modulation.env_decay.get_mut(row) {
                *decay_value = value;
            }
            let begin = self.gesture.begin(
                ValueRef::EnvDecay(target, param_id.clone()),
                RowDrag {
                    kind: RowDragKind::EnvelopeDecay,
                    param_id,
                    start_value: value,
                    range: (0.0, ENV_DECAY_MAX),
                    last_value: value,
                    last_range: (value, value),
                    last_pos: pos,
                },
                pos,
            );
            let moved = self.gesture.update(pos, ScrubValue::Scalar(value));
            return [Some(begin), moved].into_iter().flatten().collect();
        }

        if role == RowRole::AudioConfig {
            let Some((drawer, _)) = self.audio_configs.get(row).and_then(Option::as_ref) else {
                return Vec::new();
            };
            let slider_index = (0..4).find(|&index| {
                drawer.sliders.get(index).is_some_and(|slider| node == slider.track)
            });
            let Some(slider_index) = slider_index else { return Vec::new() };
            let track = tree.get_bounds(drawer.sliders[slider_index].track);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track), pos.x).clamp(0.0, 1.0);
            let (value, address, drag_kind) = match slider_index {
                0 => (
                    audio_shape_value_from_norm(AudioShapeParam::Sensitivity, norm),
                    ValueRef::AudioModShape(target, param_id.clone(), AudioShapeParam::Sensitivity),
                    RowDragKind::AudioShape(AudioShapeParam::Sensitivity),
                ),
                1 => (
                    audio_shape_value_from_norm(AudioShapeParam::Attack, norm),
                    ValueRef::AudioModShape(target, param_id.clone(), AudioShapeParam::Attack),
                    RowDragKind::AudioShape(AudioShapeParam::Attack),
                ),
                2 => (
                    audio_shape_value_from_norm(AudioShapeParam::Release, norm),
                    ValueRef::AudioModShape(target, param_id.clone(), AudioShapeParam::Release),
                    RowDragKind::AudioShape(AudioShapeParam::Release),
                ),
                _ => {
                    let mut amount = norm_to_step_amount(norm, info.spec.min, info.spec.max);
                    if info.spec.whole_numbers { amount = amount.round(); }
                    (
                        amount,
                        ValueRef::AudioModStepAmount(target, param_id.clone()),
                        RowDragKind::AudioStepAmount,
                    )
                }
            };
            match drag_kind {
                RowDragKind::AudioShape(which) => set_audio_shape_value(ctx.modulation, row, which, value),
                RowDragKind::AudioStepAmount => {
                    if let Some(audio) = ctx.modulation.audio_rows.get_mut(row) { audio.step_amount = value; }
                }
                _ => {}
            }
            let begin = self.gesture.begin(
                address,
                RowDrag {
                    kind: drag_kind,
                    param_id,
                    start_value: value,
                    range: (0.0, 1.0),
                    last_value: value,
                    last_range: (value, value),
                    last_pos: pos,
                },
                pos,
            );
            let moved = self.gesture.update(pos, ScrubValue::Scalar(value));
            return [Some(begin), moved].into_iter().flatten().collect();
        }

        let Some(slider) = self.slider_ids.get(row).and_then(Option::as_ref) else {
            return Vec::new();
        };
        let track = tree.get_bounds(slider.track);

        // Envelope target is an overlay on the same track and has priority.
        if let Some(target_ids) = self.target_ids.get(row).and_then(Option::as_ref)
            && node == target_ids.target_bar_id
        {
            let value = info.modulation.target_norm;
            let begin = self.gesture.begin(
                ValueRef::EnvelopeTarget(target, param_id.clone()),
                RowDrag {
                    kind: RowDragKind::EnvelopeTarget,
                    param_id,
                    start_value: value,
                    range: (0.0, 1.0),
                    last_value: value,
                    last_range: (value, value),
                    last_pos: pos,
                },
                pos,
            );
            return vec![begin];
        }

        let driver_range = current_trim_range(&ctx, row, TrimKind::Driver)
            .filter(|_| ctx.modulation.driver_expanded.get(row).copied().unwrap_or(false));
        if let Some((kind, is_min)) = self.trim_hit(row, node, pos, tree, driver_range) {
            let current = current_trim_range(&ctx, row, kind).unwrap_or((0.0, 1.0));
            let begin = self.gesture.begin(
                ValueRef::Trim(kind, target, param_id.clone()),
                RowDrag {
                    kind: RowDragKind::Trim { kind, is_min },
                    param_id,
                    start_value: if is_min { current.0 } else { current.1 },
                    range: current,
                    last_value: if is_min { current.0 } else { current.1 },
                    last_range: current,
                    last_pos: pos,
                },
                pos,
            );
            return vec![begin];
        }

        let is_fill = [self.trim_ids[row], self.ableton_trim_ids[row], self.audio_trim_ids[row]]
            .into_iter().flatten().any(|trim| trim.fill_id == node);
        if (node != slider.track && !is_fill) || info.spec.is_toggle || info.spec.is_trigger {
            return Vec::new();
        }
        if self.target_ids[row].is_some()
            && ctx.modulation.envelope_expanded.get(row).copied().unwrap_or(false)
        {
            let norm = ctx.modulation.target_norm.get(row).copied().unwrap_or(info.modulation.target_norm);
            let target_center = target_bar_rect(track, norm).x + TARGET_BAR_W * 0.5;
            if (pos.x - target_center).abs() < 8.0 {
                let begin = self.gesture.begin(
                    ValueRef::EnvelopeTarget(target, param_id.clone()),
                    RowDrag {
                        kind: RowDragKind::EnvelopeTarget,
                        param_id,
                        start_value: norm,
                        range: (0.0, 1.0),
                        last_value: norm,
                        last_range: (norm, norm),
                        last_pos: pos,
                    },
                    pos,
                );
                return vec![begin];
            }
        }

        let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track), pos.x);
        let mut value = BitmapSlider::normalized_to_value(norm, info.spec.min, info.spec.max);
        if info.spec.whole_numbers {
            value = value.round();
        }
        if let Some(display) = ctx.values.get_mut(row) {
            *display = value;
        }
        let begin = self.gesture.begin(
            ValueRef::Param(target, param_id.clone()),
            RowDrag {
                kind: RowDragKind::Param,
                param_id,
                start_value: value,
                range: (info.spec.min, info.spec.max),
                last_value: value,
                last_range: (value, value),
                last_pos: pos,
            },
            pos,
        );
        let moved = self.gesture.update(pos, ScrubValue::Scalar(value));
        [Some(begin), moved].into_iter().flatten().collect()
    }

    /// Track one move against the captured row address.  If a structural
    /// rebuild removed that row, the session is retained and consumed on
    /// release but no stale Move is emitted.
    pub(crate) fn handle_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        fine: bool,
        ctx: RowInteraction<'_>,
    ) -> Vec<PanelAction> {
        let Some(payload) = self.gesture.payload().cloned() else {
            return Vec::new();
        };
        let target_matches = self
            .gesture
            .address()
            .is_some_and(|address| captured_target_matches(address, ctx.target));
        if !target_matches {
            if let Some(active) = self.gesture.payload_mut() { active.last_pos = pos; }
            return Vec::new();
        }
        let Some(&row) = ctx.row_indices.get(payload.param_id.as_ref()) else {
            if let Some(active) = self.gesture.payload_mut() {
                active.last_pos = pos;
            }
            return Vec::new();
        };
        let track = match payload.kind {
            RowDragKind::Param | RowDragKind::Trim { .. } | RowDragKind::EnvelopeTarget => {
                self.slider_ids.get(row).and_then(Option::as_ref).map(|s| s.track)
            }
            RowDragKind::EnvelopeDecay => self
                .envelope_config_ids
                .get(row)
                .and_then(Option::as_ref)
                .and_then(|c| c.decay_slider.as_ref().map(|s| s.track)),
            RowDragKind::AudioShape(which) => self
                .audio_configs
                .get(row)
                .and_then(Option::as_ref)
                .and_then(|(d, _)| d.sliders.get(audio_shape_index(which)).map(|s| s.track)),
            RowDragKind::AudioStepAmount => self
                .audio_configs
                .get(row)
                .and_then(Option::as_ref)
                .and_then(|(d, _)| d.sliders.get(3).map(|s| s.track)),
        };
        let Some(track) = track else { return Vec::new() };
        let rect = tree.get_bounds(track);
        let dx = pos.x - self.gesture.start().map(|p| p.x).unwrap_or(payload.last_pos.x);
        let mut value = match payload.kind {
            RowDragKind::Param => fine_scrub_value(
                payload.start_value,
                dx,
                rect.width,
                payload.range.0,
                payload.range.1,
                fine,
            ),
            RowDragKind::EnvelopeTarget => {
                BitmapSlider::x_to_normalized(TrackSpan::of(rect), pos.x).clamp(0.0, 1.0)
            }
            RowDragKind::Trim { .. } => {
                BitmapSlider::x_to_normalized(TrackSpan::of(rect), pos.x).clamp(0.0, 1.0)
            }
            RowDragKind::EnvelopeDecay => {
                BitmapSlider::x_to_normalized(TrackSpan::of(rect), pos.x).clamp(0.0, 1.0) * ENV_DECAY_MAX
            }
            RowDragKind::AudioShape(which) => audio_shape_value_from_norm(
                which,
                BitmapSlider::x_to_normalized(TrackSpan::of(rect), pos.x).clamp(0.0, 1.0),
            ),
            RowDragKind::AudioStepAmount => {
                let norm = BitmapSlider::x_to_normalized(TrackSpan::of(rect), pos.x).clamp(0.0, 1.0);
                let mut amount = norm_to_step_amount(norm, ctx.rows[row].spec.min, ctx.rows[row].spec.max);
                if ctx.rows[row].spec.whole_numbers { amount = amount.round(); }
                amount
            }
        };
        if matches!(payload.kind, RowDragKind::Param)
            && ctx.rows[row].spec.whole_numbers
        {
            value = value.round();
        }
        let move_value = match payload.kind {
            RowDragKind::Param => {
                if let Some(display) = ctx.values.get_mut(row) {
                    *display = value;
                }
                self.push_slider_value(tree, row, value, &ctx.rows[row].spec, None);
                ScrubValue::Scalar(value)
            }
            RowDragKind::EnvelopeTarget => {
                ctx.rows[row].modulation.target_norm = value;
                if let Some(target_norm) = ctx.modulation.target_norm.get_mut(row) { *target_norm = value; }
                if let Some(target_ids) = self.target_ids.get(row).and_then(Option::as_ref) {
                    tree.set_bounds(target_ids.target_bar_id, target_bar_rect(rect, value));
                }
                ScrubValue::Scalar(value)
            }
            RowDragKind::Trim { kind, is_min } => {
                let range = clamp_trim_range(payload.last_range, value, is_min);
                match kind {
                    TrimKind::Driver => {
                        ctx.rows[row].modulation.trim_min = range.0;
                        ctx.rows[row].modulation.trim_max = range.1;
                        if let Some(trim_min) = ctx.modulation.trim_min.get_mut(row) { *trim_min = range.0; }
                        if let Some(trim_max) = ctx.modulation.trim_max.get_mut(row) { *trim_max = range.1; }
                    }
                    TrimKind::Ableton => {
                        ctx.rows[row].mapping.ableton_range = Some(range);
                    }
                    TrimKind::Audio => {
                        if let Some(audio) = ctx.modulation.audio_rows.get_mut(row) {
                            audio.range_min = range.0;
                            audio.range_max = range.1;
                        }
                    }
                }
                if let Some(active) = self.gesture.payload_mut() {
                    active.last_range = range;
                }
                let trim_ids = match kind {
                    TrimKind::Driver => self.trim_ids.get(row).and_then(Option::as_ref),
                    TrimKind::Ableton => self.ableton_trim_ids.get(row).and_then(Option::as_ref),
                    TrimKind::Audio => self.audio_trim_ids.get(row).and_then(Option::as_ref),
                };
                if let Some(trim_ids) = trim_ids {
                    reposition_trim_bars(tree, rect, trim_ids, range.0, range.1);
                }
                ScrubValue::Range(range.0, range.1)
            }
            RowDragKind::EnvelopeDecay => {
                if let Some(decay) = ctx.modulation.env_decay.get_mut(row) { *decay = value; }
                if let Some(config) = self.envelope_config_ids.get(row).and_then(Option::as_ref)
                    && let Some(slider) = config.decay_slider.as_ref()
                {
                    BitmapSlider::update_value(tree, slider, value / ENV_DECAY_MAX, &format!("{value:.2}"));
                }
                ScrubValue::Scalar(value)
            }
            RowDragKind::AudioShape(which) => {
                set_audio_shape_value(ctx.modulation, row, which, value);
                if let Some(config) = self.audio_configs.get(row).and_then(Option::as_ref)
                    && let Some(slider) = config.0.sliders.get(audio_shape_index(which))
                {
                    BitmapSlider::update_value(
                        tree,
                        slider,
                        value / audio_shape_max(which),
                        &audio_shape_value_text(which, value),
                    );
                }
                ScrubValue::Scalar(value)
            }
            RowDragKind::AudioStepAmount => {
                if let Some(audio) = ctx.modulation.audio_rows.get_mut(row) { audio.step_amount = value; }
                if let Some(config) = self.audio_configs.get(row).and_then(Option::as_ref)
                    && let Some(slider) = config.0.sliders.get(3)
                {
                    BitmapSlider::update_value(
                        tree,
                        slider,
                        step_amount_to_norm(value, ctx.rows[row].spec.min, ctx.rows[row].spec.max),
                        &if ctx.rows[row].spec.whole_numbers { format!("{value:.0}") } else { format!("{value:.2}") },
                    );
                }
                ScrubValue::Scalar(value)
            }
        };
        if let Some(active) = self.gesture.payload_mut() {
            active.last_value = value;
            active.last_pos = pos;
        }
        self.gesture.update(pos, move_value).into_iter().collect()
    }

    pub(crate) fn handle_drag_end(&mut self) -> Vec<PanelAction> {
        self.gesture.end().into_iter().collect()
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.gesture.is_active()
    }

    pub(crate) fn cancel_drag(&mut self) {
        self.gesture.cancel();
    }

    /// Reapply an in-flight live value after a structural snapshot rebuild.
    pub(crate) fn restore_live(&self, ctx: RowInteraction<'_>) {
        let Some(payload) = self.gesture.payload() else { return };
        let target_matches = self
            .gesture
            .address()
            .is_some_and(|address| captured_target_matches(address, ctx.target));
        if !target_matches {
            return;
        }
        let Some(&row) = ctx.row_indices.get(payload.param_id.as_ref()) else { return };
        match payload.kind {
            RowDragKind::Param => {
                if let Some(display) = ctx.values.get_mut(row) {
                    *display = payload.last_value;
                }
            }
            RowDragKind::EnvelopeTarget => {
                ctx.rows[row].modulation.target_norm = payload.last_value;
                if let Some(target_norm) = ctx.modulation.target_norm.get_mut(row) {
                    *target_norm = payload.last_value;
                }
            }
            RowDragKind::Trim { kind, .. } => match kind {
                TrimKind::Driver => {
                    ctx.rows[row].modulation.trim_min = payload.last_range.0;
                    ctx.rows[row].modulation.trim_max = payload.last_range.1;
                    if let Some(trim_min) = ctx.modulation.trim_min.get_mut(row) { *trim_min = payload.last_range.0; }
                    if let Some(trim_max) = ctx.modulation.trim_max.get_mut(row) { *trim_max = payload.last_range.1; }
                }
                TrimKind::Ableton => ctx.rows[row].mapping.ableton_range = Some(payload.last_range),
                TrimKind::Audio => {
                    if let Some(audio) = ctx.modulation.audio_rows.get_mut(row) {
                        audio.range_min = payload.last_range.0;
                        audio.range_max = payload.last_range.1;
                    }
                }
            }
            RowDragKind::EnvelopeDecay => {
                if let Some(decay) = ctx.modulation.env_decay.get_mut(row) {
                    *decay = payload.last_value;
                }
            }
            RowDragKind::AudioShape(which) => {
                set_audio_shape_value(ctx.modulation, row, which, payload.last_value);
            }
            RowDragKind::AudioStepAmount => {
                if let Some(audio) = ctx.modulation.audio_rows.get_mut(row) {
                    audio.step_amount = payload.last_value;
                }
            }
        }
    }

    pub(crate) fn active_param_index(&self, target: &GraphParamTarget, rows: &[ParamRow]) -> Option<usize> {
        let payload = self.gesture.payload()?;
        if !matches!(payload.kind, RowDragKind::Param)
            || !captured_target_matches(self.gesture.address()?, target)
        {
            return None;
        }
        rows.iter().position(|row| row.id == payload.param_id)
    }

    pub(crate) fn active_param_value(
        &self,
        target: &GraphParamTarget,
        id: &manifold_foundation::ParamId,
    ) -> Option<f32> {
        let payload = self.gesture.payload()?;
        let ValueRef::Param(active_target, active_id) = self.gesture.address()? else { return None };
        if active_target == target && active_id == id && matches!(payload.kind, RowDragKind::Param) {
            Some(payload.last_value)
        } else {
            None
        }
    }

    /// Populate `self.row_index` for row `i` from the per-row node-id fields
    /// that were just built — the SAME fields the row renders (D5: routing
    /// agrees with rendering by construction). Called once per row from both
    /// the toggle/trigger and slider row builders, right after their fields
    /// land. Bundles register EVERY interactive node they own under one role
    /// (the widget-contract split — `row_action`'s bundle `resolve` methods
    /// name the sub-element).
    pub(crate) fn reindex_row(&mut self, tree: &UITree, i: usize) {
        if let Some(s) = &self.slider_ids[i] {
            self.row_index.insert(tree.widget_of(s.track), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(s.value_text), i, RowRole::Slider);
            if let Some(l) = s.label {
                self.row_index.insert(tree.widget_of(l), i, RowRole::Slider);
            }
        }
        // Trim/target overlay handles nest under the slider track (they
        // inherit its stability through the parent chain — D4) and belong to
        // the slider bundle functionally; indexed under the same role so
        // `handle_pointer_down` resolves them by row instead of scanning.
        if let Some(t) = &self.trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.ableton_trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.audio_trim_ids[i] {
            self.row_index.insert(tree.widget_of(t.fill_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.min_bar_id), i, RowRole::Slider);
            self.row_index.insert(tree.widget_of(t.max_bar_id), i, RowRole::Slider);
        }
        if let Some(t) = &self.target_ids[i] {
            self.row_index.insert(tree.widget_of(t.target_bar_id), i, RowRole::Slider);
        }
        if let Some(rc) = self.row_catcher_ids[i] {
            self.row_index.insert(tree.widget_of(rc), i, RowRole::RowCatcher);
        }
        if let Some(b) = self.driver_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::DriverBtn);
        }
        if let Some(b) = self.envelope_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::EnvelopeBtn);
        }
        if let Some(b) = self.audio_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::AudioBtn);
        }
        if let Some(b) = self.automation_btn_ids[i] {
            self.row_index.insert(tree.widget_of(b), i, RowRole::AutomationBtn);
        }
        if let Some(t) = &self.toggle_ids[i] {
            self.row_index.insert(tree.widget_of(t.button_id), i, RowRole::ToggleBtn);
            if let Some(l) = t.label_id {
                self.row_index.insert(tree.widget_of(l), i, RowRole::Label);
            }
        }
        if let Some(c) = &self.driver_config_ids[i] {
            for &b in c.button_ids() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::DriverConfig);
            }
        }
        if let Some(c) = &self.envelope_config_ids[i] {
            if let Some(ds) = &c.decay_slider {
                self.row_index.insert(tree.widget_of(ds.track), i, RowRole::EnvelopeConfig);
                self.row_index.insert(tree.widget_of(ds.value_text), i, RowRole::EnvelopeConfig);
                if let Some(l) = ds.label {
                    self.row_index.insert(tree.widget_of(l), i, RowRole::EnvelopeConfig);
                }
            }
            for &b in c.drawer.button_ids() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::EnvelopeConfig);
            }
            if let Some(step_slider) = &c.step_slider {
                self.row_index.insert(tree.widget_of(step_slider.track), i, RowRole::EnvelopeConfig);
                self.row_index.insert(tree.widget_of(step_slider.value_text), i, RowRole::EnvelopeConfig);
                if let Some(l) = step_slider.label {
                    self.row_index.insert(tree.widget_of(l), i, RowRole::EnvelopeConfig);
                }
            }
        }
        if let Some(c) = &self.ableton_config_ids[i] {
            for &b in c.button_ids() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::AbletonConfig);
            }
        }
        if let Some((dids, _)) = &self.audio_configs[i] {
            for &b in dids.button_ids() {
                self.row_index.insert(tree.widget_of(b), i, RowRole::AudioConfig);
            }
            for s in &dids.sliders {
                self.row_index.insert(tree.widget_of(s.track), i, RowRole::AudioConfig);
                self.row_index.insert(tree.widget_of(s.value_text), i, RowRole::AudioConfig);
                if let Some(l) = s.label {
                    self.row_index.insert(tree.widget_of(l), i, RowRole::AudioConfig);
                }
            }
        }
        if let Some(c) = self.mapping_chevron_ids[i] {
            self.row_index.insert(tree.widget_of(c), i, RowRole::MappingChevron);
        }
        for &(node, _tab) in &self.mod_tab_ids[i] {
            self.row_index.insert(tree.widget_of(node), i, RowRole::ModTab);
        }
    }

    /// Resolve a trim press for one row. Exact bar hits are checked first in
    /// driver/Ableton/audio order; when the press is on the slider track, the
    /// driver's handles retain the existing proximity catch-zone. Callers
    /// check higher-priority envelope target hits before calling this helper.
    /// The driver range is only used for proximity geometry and lets scene
    /// cards use their own live modulation state without duplicating the hit
    /// test.
    pub(crate) fn trim_hit(
        &self,
        row: usize,
        node_id: NodeId,
        pos: Vec2,
        tree: &UITree,
        driver_range: Option<(f32, f32)>,
    ) -> Option<(TrimKind, bool)> {
        let trim_ids = [
            self.trim_ids.get(row).and_then(Option::as_ref),
            self.ableton_trim_ids.get(row).and_then(Option::as_ref),
            self.audio_trim_ids.get(row).and_then(Option::as_ref),
        ];
        let kinds = [TrimKind::Driver, TrimKind::Ableton, TrimKind::Audio];

        // Exact hits always win over proximity, including when two overlays
        // occupy the same x coordinate. This preserves the inspector's
        // envelope-target-before-trim ordering at the call site and its
        // established trim probe order here.
        for (kind, ids) in kinds.into_iter().zip(trim_ids) {
            let Some(ids) = ids else { continue };
            if node_id == ids.min_bar_id {
                return Some((kind, true));
            }
            if node_id == ids.max_bar_id {
                return Some((kind, false));
            }
        }

        // Preserve the inspector's established feel-zone semantics: driver
        // handles are reachable by proximity on their track; Ableton/audio
        // remain exact-bar hits so their overlapping overlays stay explicit.
        let (driver_min, driver_max) = driver_range?;
        trim_ids[0]?;
        let slider = self.slider_ids.get(row).and_then(Option::as_ref)?;
        let is_overlay_fill = trim_ids.iter().flatten().any(|ids| node_id == ids.fill_id);
        if node_id != slider.track && !is_overlay_fill {
            return None;
        }
        let bars = trim_bar_rects(tree.get_bounds(slider.track), driver_min, driver_max);
        let min_center = bars.min_bar.x + TRIM_BAR_W * 0.5;
        let max_center = bars.max_bar.x + TRIM_BAR_W * 0.5;
        let hit_zone = 8.0;
        let dist_min = (pos.x - min_center).abs();
        let dist_max = (pos.x - max_center).abs();
        if dist_min < hit_zone && dist_min <= dist_max {
            return Some((TrimKind::Driver, true));
        }
        if dist_max < hit_zone {
            return Some((TrimKind::Driver, false));
        }
        None
    }

    /// section 5.6 shared per-row value push: normalize → format → update row `i`'s
    /// slider fill + readout. The ONE place both parameter cards write a
    /// slider value (`ParamCardPanel::sync_param_value` and
    /// `ScenePanel::sync_properties_values`), so the fill/readout math can't
    /// drift apart. `display_norm_override` draws the FILL at an in-flight
    /// snapback position while the readout still shows the true `value`; `None`
    /// draws `value`'s own normalized position. No-op for a row with no slider
    /// bundle (a toggle/trigger row, or an out-of-range index).
    pub(crate) fn push_slider_value(
        &self,
        tree: &mut UITree,
        i: usize,
        value: f32,
        spec: &RowSpec,
        display_norm_override: Option<f32>,
    ) {
        let Some(ids) = self.slider_ids.get(i).and_then(|s| s.as_ref()) else {
            return;
        };
        let norm = crate::slider::BitmapSlider::value_to_normalized(value, spec.min, spec.max);
        let text = format_param_value(
            value,
            spec.min,
            spec.whole_numbers,
            spec.is_angle,
            spec.value_labels.as_deref(),
        );
        crate::slider::BitmapSlider::update_value(tree, ids, display_norm_override.unwrap_or(norm), &text);
    }

    /// Replay every materialised slider's `Track + RightClick → reset` intent —
    /// main rows AND every drawer slider (audio-shape Amount/Attack/Release,
    /// envelope Decay). This is the ROW half of the card's intent
    /// registration; the owning panel's `register_intents` calls it and then
    /// adds the card-chrome intents (border claim, relight resets, per-param
    /// mapping menus) it keeps. Mirrors `SceneCardState::register_intents`.
    pub(crate) fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::slider::BitmapSlider;
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let (Some(ids), Some(reset)) =
                (slider, self.slider_resets.get(pi).and_then(|r| r.as_ref()))
            {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }
        for cfg in self.driver_config_ids.iter().flatten() {
            cfg.register_intents(intents);
        }
        for cfg in self.envelope_config_ids.iter().flatten() {
            cfg.drawer.register_intents(intents);
        }
        for cfg in self.ableton_config_ids.iter().flatten() {
            cfg.register_intents(intents);
        }
        for cfg in self.envelope_config_ids.iter().flatten() {
            if let Some((ds, dr)) = cfg.decay_slider.as_ref().zip(cfg.decay_reset.as_ref()) {
                BitmapSlider::register_track_reset(ds, dr, intents);
            }
            if let Some((step_slider, step_reset)) = cfg.step_slider.as_ref().zip(cfg.step_reset.as_ref()) {
                BitmapSlider::register_track_reset(step_slider, step_reset, intents);
            }
        }
        for cfg in self.audio_configs.iter().flatten() {
            let (dids, _) = cfg;
            dids.register_intents(intents);
            for (sl, reset) in dids.sliders.iter().zip(dids.slider_resets.iter()) {
                BitmapSlider::register_track_reset(sl, reset, intents);
            }
        }
    }

    // ── Row-action routing ────────────────────────────────────────
    //
    // `row_action` and its helpers turn a resolved `(row, role)` hit into a
    // `PanelAction`. `RowHost` owns the id bundles the routing reads
    // (`slider_ids`/`driver_config_ids`/`audio_configs`/…); the per-row *model*
    // it needs — the `ParamRow`s, the `ParamModState`, the value/osc caches and
    // the drawer-tab / section-fold UI state — belongs to the owning panel and
    // is passed in by reference. That keeps `RowHost` model-free while a single
    // routing body serves every card kind (the `SceneCardState` twin P-S3
    // folds in).

    /// Point param `pi`'s config drawer at `tab` — used when arming a modulator
    /// so its config comes forward. No-op if `pi` is out of range. Writes the
    /// panel-owned `mod_active_tab` (RowHost holds ids/routing, not the tab
    /// choice the render pass reads).
    fn focus_mod_tab(&self, mod_active_tab: &mut [ModTab], pi: usize, tab: ModTab) {
        if let Some(slot) = mod_active_tab.get_mut(pi) {
            *slot = tab;
        }
    }

    /// BUG-250: map a `RowRole::Slider` value-cell hit (an enum row) to the
    /// shared cycle-or-dropdown action set (`enum_value_cell_actions`). The
    /// cell node id comes from the row's own slider ids (the dropdown anchors
    /// under it); the current value is the synced base value, matching what
    /// the cell displays.
    fn enum_value_cell_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        clicked: NodeId,
        rows: &[ParamRow],
        base_values: &[f32],
    ) -> Vec<PanelAction> {
        let info = &rows[pi];
        let labels = info.spec.value_labels.clone().unwrap_or_default();
        let cell = self
            .slider_ids
            .get(pi)
            .and_then(|s| s.as_ref())
            .map(|s| s.value_text)
            .unwrap_or(clicked);
        let value = base_values.get(pi).copied().unwrap_or(info.spec.default);
        enum_value_cell_actions(target, rows[pi].id.clone(), &labels, value, info.spec.min, cell)
    }

    /// The "A" audio-mod button action — always opens (arms) or closes
    /// (disarms) this param's audio drawer, never the Audio Setup modal. With
    /// no sends defined yet, arming auto-creates the project's first send and
    /// points this param at it in one undo step, so the drawer opens populated
    /// and ready (the user routes/renames sends in Audio Setup afterward). The
    /// drawer's own "+" adds further sends.
    fn audio_toggle_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        rows: &[ParamRow],
        mod_state: &ParamModState,
    ) -> Vec<PanelAction> {
        let ms = mod_state;
        if ms.audio_rows.get(pi).is_some_and(|row| row.active) {
            // Already armed → disarm (closes the drawer), regardless of sends.
            vec![PanelAction::Modulation(ModulationAction::AudioModToggle(target, rows[pi].id.clone()))]
        } else if ms.audio_sends.is_empty() {
            // Not armed, no send to point at → open Audio Setup so the user can
            // create one. Sends are defined there, never from the drawer.
            vec![PanelAction::Root(RootAction::OpenAudioSetup)]
        } else {
            // Not armed, sends exist → arm at the project's first send.
            vec![PanelAction::Modulation(ModulationAction::AudioModToggle(target, rows[pi].id.clone()))]
        }
    }

    /// Build an `AudioModSetSource` from the param's current selections, with
    /// one axis optionally overridden (the clicked send / feature-kind / band).
    /// Empty when no send resolves (nothing to point at).
    fn audio_set_source_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        send_override: Option<usize>,
        kind_override: Option<usize>,
        band_override: Option<usize>,
        rows: &[ParamRow],
        mod_state: &ParamModState,
    ) -> Vec<PanelAction> {
        let ms = mod_state;
        let send_k = send_override.map(|k| k as i32).unwrap_or_else(|| ms.audio_send_index(pi));
        let Some(send_id) = (send_k >= 0)
            .then(|| ms.audio_sends.get(send_k as usize).map(|send| send.id.clone()))
            .flatten()
        else {
            return vec![];
        };
        let kind_idx = kind_override.unwrap_or_else(|| ms.audio_rows.get(pi).map_or(0, |row| row.kind_idx) as usize);
        let band_idx = band_override.unwrap_or_else(|| ms.audio_rows.get(pi).map_or(0, |row| row.band_idx) as usize);
        let feature = crate::types::AudioFeature::new(
            audio_kind_from_index(kind_idx),
            audio_band_from_index(band_idx),
        );
        vec![PanelAction::Modulation(ModulationAction::AudioModSetSource(target, rows[pi].id.clone(), send_id, feature))]
    }

    pub(crate) fn audio_drawer_action(
        &self,
        target: GraphParamTarget,
        row: usize,
        click: crate::panels::AudioDrawerClick,
        rows: &[ParamRow],
        mod_state: &mut ParamModState,
    ) -> Vec<PanelAction> {
        use crate::panels::AudioDrawerClick::*;
        match click {
            Send(k) => self.audio_set_source_action(target, row, Some(k), None, None, rows, mod_state),
            Feature(feature) => {
                let kind = crate::types::AudioFeatureKind::ALL.iter().position(|&v| v == feature.kind).unwrap_or(0);
                let band = crate::types::AudioBand::ALL.iter().position(|&v| v == feature.band).unwrap_or(0);
                self.audio_set_source_action(target, row, None, Some(kind), Some(band), rows, mod_state)
            }
            Custom => {
                if let Some(open) = mod_state.audio_matrix_open.get_mut(row) { *open = !*open; }
                vec![PanelAction::Params(ParamsAction::ModConfigTabChanged)]
            }
            Kind(k) => self.audio_set_source_action(target, row, None, Some(k), None, rows, mod_state),
            Band(b) => self.audio_set_source_action(target, row, None, None, Some(b), rows, mod_state),
            Invert => vec![PanelAction::Modulation(ModulationAction::AudioModSetInvert(target, rows[row].id.clone()))],
            TriggerMode(m) => self.audio_set_trigger_mode_action(target, row, m, rows),
            Action(k) => vec![PanelAction::Modulation(ModulationAction::AudioModSetActionKind(target, rows[row].id.clone(), k))],
            Wrap(w) => vec![PanelAction::Modulation(ModulationAction::AudioModSetWrap(target, rows[row].id.clone(), w))],
        }
    }

    /// A click on an `is_trigger_gate` row's Mode row (section 9 U3) — converts the
    /// clicked button index to a `TriggerFireMode` at this dispatch boundary
    /// and issues one `AudioModSetTriggerMode`, the same command family every
    /// other audio-mod drawer edit uses.
    fn audio_set_trigger_mode_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        mode_idx: usize,
        rows: &[ParamRow],
    ) -> Vec<PanelAction> {
        vec![PanelAction::Modulation(ModulationAction::AudioModSetTriggerMode(target, rows[pi].id.clone(), mode_idx))]
    }

    /// If `node_id` is a numeric param's value cell, build the
    /// `BeginParamTextInput` action that opens a type-in box for it — target +
    /// id, the cell's anchor rect, the base value to prefill, the clamp range,
    /// and the int-rounding flag. Returns `None` for non-value-cell nodes and
    /// for enum rows (`value_labels` use the dropdown, not text); toggle/
    /// trigger rows carry no slider bundle, so they never match.
    ///
    /// Shared so the effect/generator card and the scene properties card resolve
    /// the SAME double-click gesture through the SAME code (D13/D14) — the scene
    /// panel routes its rows here instead of carrying a `value_cell_typein` copy.
    pub(crate) fn value_cell_typein(
        &self,
        node_id: NodeId,
        tree: &UITree,
        rows: &[ParamRow],
        base_values: &[f32],
        target: GraphParamTarget,
    ) -> Option<PanelAction> {
        debug_assert_eq!(
            crate::slider::BitmapSlider::intent_for(
                crate::slider::SliderZone::ValueCell,
                crate::intent::Gesture::DoubleClick
            ),
            Some(crate::slider::SliderIntent::EditValue),
            "value_cell_typein is the contract's ValueCell+DoubleClick->EditValue translation (D13/D14)"
        );
        for (pi, slot) in self.slider_ids.iter().enumerate() {
            let Some(ids) = slot else { continue };
            if ids.value_text != node_id {
                continue;
            }
            let info = rows.get(pi)?;
            if info.spec.value_labels.is_some() {
                return None;
            }
            return Some(PanelAction::Root(RootAction::BeginParamTextInput {
                target,
                param_id: rows[pi].id.clone(),
                anchor: tree.get_bounds(ids.value_text),
                value: base_values.get(pi).copied().unwrap_or(info.spec.default),
                min: info.spec.min,
                max: info.spec.max,
                whole_numbers: info.spec.whole_numbers,
                degrees: info.spec.is_angle,
            }));
        }
        None
    }

    /// Route a resolved `(row, role)` hit to the `PanelAction` the old per-kind
    /// gauntlets emitted for that element — ONE match, both card kinds (D5).
    /// Identity comes off `rows[row].id`; the wire target is the caller's
    /// `param_target()`, passed in. Bundle roles (`DriverConfig`/`AbletonConfig`/
    /// `AudioConfig`) delegate to the bundle's own `resolve` (the
    /// widget-contract split: the bundle knows its own nodes, this function only
    /// knows which row it belongs to). Reads only `RowHost`'s id bundles; every
    /// mutation lands on the panel-owned model passed by `&mut`.
    ///
    /// The wide parameter list threads in the card model (the `rows`,
    /// `mod_state`, and the value/osc/tab/fold state) that stays on the panel
    /// this phase; it collapses when P-S3 folds `rows`/`mod_state` into
    /// `RowHost` (the `SceneCardState` unification). Well under `clippy.toml`'s
    /// `too-many-arguments-threshold` (20).
    pub(crate) fn row_action(
        &self,
        target: GraphParamTarget,
        row: usize,
        role: RowRole,
        node: NodeId,
        rows: &[ParamRow],
        base_values: &[f32],
        osc_addresses: &[Option<String>],
        mod_state: &mut ParamModState,
        mod_active_tab: &mut [ModTab],
        copied_flash: &mut CopyToClipboardLabelState,
        section_folded: &mut ahash::AHashMap<String, bool>,
    ) -> Vec<PanelAction> {
        match role {
            RowRole::Slider => {
                let Some(ids) = self.slider_ids[row] else {
                    return Vec::new();
                };
                if ids.label == Some(node) {
                    if osc_addresses.get(row).and_then(|a| a.as_ref()).is_none() {
                        return Vec::new();
                    }
                    if let Some(label) = ids.label {
                        copied_flash.trigger(label);
                    }
                    let addr = osc_addresses[row].clone().unwrap_or_default();
                    return vec![PanelAction::Root(RootAction::CopyOscAddress(addr))];
                }
                if ids.value_text == node && rows[row].spec.value_labels.is_some() {
                    return self.enum_value_cell_action(target, row, node, rows, base_values);
                }
                // The track itself (drag start) and a plain numeric value cell
                // (double-click type-in, a different dispatch path) emit no
                // click action — matches the old gauntlet's fall-through.
                Vec::new()
            }
            RowRole::RowCatcher => Vec::new(),
            RowRole::Label => {
                // Toggle/trigger row label → copy OSC address (mirrors the
                // slider label path; toggle rows carry no slider bundle).
                if let Some(addr) = osc_addresses.get(row).and_then(|a| a.clone()) {
                    copied_flash.trigger(node);
                    return vec![PanelAction::Root(RootAction::CopyOscAddress(addr))];
                }
                Vec::new()
            }
            RowRole::DriverBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Driver);
                vec![PanelAction::Modulation(ModulationAction::DriverToggle(target, rows[row].id.clone()))]
            }
            RowRole::EnvelopeBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Envelope);
                vec![PanelAction::Modulation(ModulationAction::EnvelopeToggle(target, rows[row].id.clone()))]
            }
            RowRole::AudioBtn => {
                self.focus_mod_tab(mod_active_tab, row, ModTab::Audio);
                self.audio_toggle_action(target, row, rows, mod_state)
            }
            RowRole::AutomationBtn => {
                vec![PanelAction::Params(ParamsAction::ShowAutomation(target, rows[row].id.clone()))]
            }
            RowRole::ToggleBtn => {
                let is_trigger = rows.get(row).map(|i| i.spec.is_trigger).unwrap_or(false);
                let pid = rows[row].id.clone();
                if is_trigger {
                    vec![PanelAction::Params(ParamsAction::ParamFire(target, pid))]
                } else {
                    vec![PanelAction::Params(ParamsAction::ParamToggle(target, pid))]
                }
            }
            RowRole::DriverConfig => {
                let Some(cfg) = &self.driver_config_ids[row] else {
                    return Vec::new();
                };
                cfg.resolve_action(node).cloned().into_iter().collect()
            }
            // The Decay/Step slider's own click (drag start / value-cell type-in)
            // carries no left-click action — matches the old gauntlet, which
            // never checked envelope-config nodes in `handle_click`. Action/Wrap
            // button clicks are resolved below.
            RowRole::EnvelopeConfig => {
                let Some(cfg) = &self.envelope_config_ids[row] else {
                    return Vec::new();
                };
                cfg.drawer.resolve_action(node).cloned().into_iter().collect()
            }
            RowRole::AudioConfig => {
                let Some((dids, _send_count)) = self.audio_configs[row].as_ref() else {
                    return Vec::new();
                };
                if let Some(PanelAction::Root(RootAction::AudioDrawerClick(_, _, click))) = dids.resolve_action(node).cloned() {
                    self.audio_drawer_action(target, row, click, rows, mod_state)
                } else {
                    dids.resolve_action(node).cloned().into_iter().collect()
                }
            }
            RowRole::AbletonConfig => {
                let Some(cfg) = &self.ableton_config_ids[row] else {
                    return Vec::new();
                };
                cfg.resolve_action(node).cloned().into_iter().collect()
            }
            RowRole::ModTab => {
                let Some(&(_, tab)) = self.mod_tab_ids[row].iter().find(|(n, _)| *n == node) else {
                    return Vec::new();
                };
                if let Some(slot) = mod_active_tab.get_mut(row) {
                    *slot = tab;
                }
                vec![PanelAction::Params(ParamsAction::ModConfigTabChanged)]
            }
            RowRole::MappingChevron => vec![PanelAction::Root(RootAction::OpenCardMapping {
                target,
                param_id: rows[row].id.clone(),
                anchor_node_id: node,
            })],
            RowRole::SectionHeader => {
                let Some(name) = self
                    .section_header_ids
                    .iter()
                    .find(|(hid, _)| *hid == node)
                    .map(|(_, n)| n.clone())
                else {
                    return Vec::new();
                };
                let folded = section_folded.entry(name).or_insert(false);
                *folded = !*folded;
                vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]
            }
            RowRole::RelightToggle | RowRole::RelightHeightBtn | RowRole::RelightSlider => {
                // Never inserted into `row_index` (relight has no `rows` slot) —
                // the top-of-`handle_click` checks own these. Kept here only for
                // match exhaustiveness.
                Vec::new()
            }
            RowRole::ColourSwatch(_)
            | RowRole::MaterialFeatureToggle(_)
            | RowRole::MaterialPlacement(_) => Vec::new(),
        }
    }
}
