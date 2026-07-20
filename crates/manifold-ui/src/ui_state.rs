// UI-specific state for timeline editor (session-only, not serialized).
// Tracks selection, drag state, hover, and zoom.
//
// Mechanical 1:1 port of Unity UIState.cs.
// Replaces the former app::SelectionState + app::ClipDragState.

use crate::panels::InspectorTab;
use crate::view::{SelectionRegion, UiAutomationPointRef, UiGraphTarget};
use manifold_foundation::{Beats, ClipId, LayerId, MarkerId, ParamId};
use std::collections::{HashMap, HashSet};

/// D1 (`docs/TIMELINE_INTERACTION_P1_SPEC.md`): the single timeline-selection
/// authority. Exactly one kind is active at any moment — the enum makes the
/// old "clip-id set AND an `is_active` region flag simultaneously" state
/// unrepresentable, which is the P0 bug class this replaces. The
/// `SelectionRegion` struct survives inside `TimeRange`; what died is
/// `is_active` as an independent stored flag that gestures forgot to clear.
/// Derived conveniences (region bounds of a clip selection, etc.) are computed
/// on demand by callers that have the project, never stored here.
/// What "the same selection" means for the inspector tab pin: the
/// primary layer, primary clip, and the layer selection set. Two syncs with
/// an equal tuple are the same selection even if `selection_version` moved
/// between them (a command side effect touched the version, not the
/// selection). Cheap — no allocation beyond the existing `HashSet` clone.
type SelectionIdentity = (Option<LayerId>, Option<ClipId>, HashSet<LayerId>);

#[derive(Debug, Clone, Default)]
pub enum TimelineSelection {
    #[default]
    None,
    /// Clip selection — a set of whole clips. `anchor` is the last-clicked
    /// clip, the fixed end a future shift-range extends from (P1.3 consumes it;
    /// until then it simply tracks the primary clip).
    Clips {
        ids: HashSet<ClipId>,
        anchor: Option<ClipId>,
    },
    /// Time-range selection — a beat × layer region.
    TimeRange(SelectionRegion),
}

pub struct UIState {
    // ── Selection (single authority — D1) ──
    /// Clip-set XOR time-range XOR nothing. Replaces the former
    /// `selected_clip_ids` field + `selection_region.is_active` flag pair.
    selection: TimelineSelection,

    /// Monotonically increasing counter — bumped on every selection/hover change.
    /// Used for cheap dirty-checking by viewport and layer headers.
    pub selection_version: u64,

    /// Most recently selected clip ID (for footer info, property display).
    pub primary_selected_clip_id: Option<ClipId>,

    /// LayerId of the layer the primary selected clip is on.
    pub selected_layer_id_for_clip: Option<LayerId>,

    // ── Layer Selection (mutually exclusive with clip selection) ──
    pub selected_layer_ids: HashSet<LayerId>,

    /// Most recently selected layer ID (for inspector display).
    pub primary_selected_layer_id: Option<LayerId>,

    // ── Cursor Position (updated on mouse move, used for paste target) ──
    pub cursor_beat: f32,
    pub cursor_layer_id: Option<LayerId>,

    // ── Insert Cursor (set on click, persists until next click or region drag) ──
    pub insert_cursor_beat: Option<Beats>,
    pub insert_cursor_layer_id: Option<LayerId>,

    // ── Hover ──
    pub hovered_clip_id: Option<ClipId>,

    // Transient drag/trim/scrub state is NOT here — it has a single owner,
    // `InteractionOverlay` (the timeline interaction component). UIState holds
    // only the persistent selection/cursor/zoom the renderer reads. See
    // `docs/TIMELINE_API_DESIGN.md` §3.3.

    // ── Zoom ──
    pub current_zoom_index: usize,

    // ── Marker Selection ──
    pub selected_marker_ids: HashSet<MarkerId>,

    // ── Inspector scope ──
    /// The inspector scope the user pinned by clicking a tab, plus the
    /// selection *identity* it was pinned under (BUG-266: `selection_version`
    /// alone was too coarse — command side effects like add-effect's
    /// behind-the-scenes selection touch bump it without changing WHICH
    /// layer/clip is selected, and used to kill the pin along with them). The
    /// pin is a *view* over the live timeline selection: clicking a tab points
    /// the inspector at that rung (Clip / Layer / Group / Master) WITHOUT
    /// changing what's selected, so the full ownership chain stays available
    /// and you keep your place. It clears only on a genuine identity change —
    /// see `pinned_scope()`. This is the one piece of inspector state that
    /// isn't pure selection. See docs/UI_LAYOUT_DESIGN.md.
    scope_pin: Option<(InspectorTab, SelectionIdentity)>,

    /// Automation-mode view toggle (Live's `A`, P4 `docs/AUTOMATION_LANES_DESIGN.md`
    /// §7): show/hide lane strips across the timeline. Purely a view-state
    /// bool — never serialized, never routed through `EditingService` — but it
    /// DOES change the Y-layout (a visible lane grows its track), so any
    /// toggle must also mark the app's structural-sync dirty flag; see
    /// `ui_bridge::transport::dispatch_transport`'s `PanelAction::
    /// ToggleAutomationMode` arm.
    pub automation_mode_visible: bool,

    /// The single selected breakpoint, for the Delete key (P4 Unit A —
    /// marquee multi-select is a later unit; `docs/AUTOMATION_LANES_DESIGN.md`
    /// §7's "Marquee-select multiple dots and drag/delete them together").
    /// Set on a plain click on an existing dot; cleared on any other
    /// selection change or when automation mode toggles off. Never
    /// serialized — pure view/interaction state, same tier as
    /// `hovered_clip_id`.
    pub selected_automation_point: Option<UiAutomationPointRef>,

    /// Marquee (rubber-band) multi-selection of automation breakpoints (P4
    /// Unit B, `docs/AUTOMATION_LANES_DESIGN.md` §7's "Marquee-select
    /// multiple dots and drag/delete them together"). Populated live during
    /// an `AutomationMarquee` drag; Delete removes the whole set as one undo
    /// entry, a drag starting on a member moves the whole set together. Same
    /// tier as `selected_automation_point` — view/interaction state, never
    /// serialized.
    pub selected_automation_points: Vec<UiAutomationPointRef>,

    /// Pencil/draw mode toggle (Live's `B`) — while on, dragging inside an
    /// automation lane strip draws a point at each grid step instead of
    /// grabbing a dot/segment (P4 Unit B, §7's "Draw mode"). Only meaningful
    /// while `automation_mode_visible`; never serialized.
    pub automation_draw_mode: bool,

    /// The param most recently touched (slider grab, inspector knob drag) on
    /// each layer — independent of whether it has a real `AutomationLane`
    /// yet (P5, `docs/AUTOMATION_LANES_DESIGN.md` §7 addendum's
    /// "touch-to-select" + "first-draw path"). While automation mode is
    /// visible, a chosen param with no backing lane renders as a flat line
    /// at its current base value — Live's "every param has an implicit
    /// envelope" feel — and the first click on that line creates the real
    /// lane via the existing `AddAutomationPointCommand` path (no new
    /// command). One entry per layer; touching a different param replaces
    /// it. Harmless once the lane is real: `ui_translate` skips the
    /// placeholder once a real enabled lane exists for the same
    /// `(target, param_id)`, so a stale entry here never double-draws.
    /// Never serialized — pure view state, same tier as
    /// `automation_mode_visible`.
    pub chosen_automation_params: HashMap<LayerId, (UiGraphTarget, ParamId)>,
}

impl Default for UIState {
    fn default() -> Self {
        Self::new()
    }
}

impl UIState {
    pub fn new() -> Self {
        Self {
            selection: TimelineSelection::None,
            selection_version: 0,
            primary_selected_clip_id: None,
            selected_layer_id_for_clip: None,
            selected_layer_ids: HashSet::new(),
            primary_selected_layer_id: None,
            cursor_beat: 0.0,
            cursor_layer_id: None,
            insert_cursor_beat: None,
            insert_cursor_layer_id: None,
            hovered_clip_id: None,
            current_zoom_index: crate::color::DEFAULT_ZOOM_INDEX,
            selected_marker_ids: HashSet::new(),
            scope_pin: None,
            automation_mode_visible: false,
            selected_automation_point: None,
            selected_automation_points: Vec::new(),
            automation_draw_mode: false,
            chosen_automation_params: HashMap::new(),
        }
    }

    /// Touch-to-select (§7 addendum): record `target`/`param_id` as the
    /// layer's active automation-chooser selection. Called from the same
    /// funnel every param drag already goes through
    /// (`PanelAction::ParamSnapshot`'s handler, `ui_bridge/inspector.rs`) —
    /// fires once per touch, not per drag-frame.
    pub fn set_chosen_automation_param(
        &mut self,
        layer_id: LayerId,
        target: UiGraphTarget,
        param_id: ParamId,
    ) {
        self.chosen_automation_params
            .insert(layer_id, (target, param_id));
    }

    // ── Inspector scope ─────────────────────────────────────────────

    /// The current selection identity — see `SelectionIdentity`.
    fn selection_identity(&self) -> SelectionIdentity {
        (
            self.primary_selected_layer_id.clone(),
            self.primary_selected_clip_id.clone(),
            self.selected_layer_ids.clone(),
        )
    }

    fn identity_is_empty(identity: &SelectionIdentity) -> bool {
        identity.0.is_none() && identity.1.is_none() && identity.2.is_empty()
    }

    /// The inspector scope currently pinned by a tab click, if the pin is still
    /// live. `None` ⇒ fall back to the selection-derived default scope.
    ///
    /// live means selection *identity*, not `selection_version` — a
    /// version bump alone (a command side effect, e.g. add-effect's
    /// behind-the-scenes selection touch) does not clear the pin. It clears
    /// only when the identity changes to a different, NON-EMPTY value; a
    /// transient empty selection (clear-then-reselect churn) holds the pin so
    /// it can reassert once the identity returns.
    pub fn pinned_scope(&self) -> Option<InspectorTab> {
        let (tab, pinned_identity) = self.scope_pin.as_ref()?;
        let current = self.selection_identity();
        if Self::identity_is_empty(&current) || &current == pinned_identity {
            Some(*tab)
        } else {
            None
        }
    }

    /// Pin the inspector to a scope (a tab click) WITHOUT touching the timeline
    /// selection. Bumps `selection_version` so the inspector rebuilds; the pin
    /// is recorded against the current selection identity — see
    /// `pinned_scope()` for when it clears. Idempotent for the active scope.
    pub fn pin_scope(&mut self, tab: InspectorTab) {
        if self.pinned_scope() == Some(tab) {
            return;
        }
        self.selection_version += 1;
        self.scope_pin = Some((tab, self.selection_identity()));
    }

    /// Release any scope pin without changing the timeline selection, so the
    /// inspector falls back to the selection-derived scope. Bumps
    /// `selection_version` (triggering a rebuild) only if a pin was set.
    pub fn clear_scope_pin(&mut self) {
        if self.scope_pin.take().is_some() {
            self.selection_version += 1;
        }
    }

    // ── Clip Selection ──────────────────────────────────────────────

    /// Select a single clip (clears previous selection and region). Called on normal click.
    /// Unity UIState.cs SelectClip (lines 167-178).
    pub fn select_clip(&mut self, clip_id: ClipId, layer_id: LayerId) {
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.clear_layer_selection();
        let mut ids = HashSet::new();
        ids.insert(clip_id.clone());
        // Replacing `selection` inherently drops any active region — the whole
        // point of D1: no gesture can leave a stale region behind.
        self.selection = TimelineSelection::Clips {
            ids,
            anchor: Some(clip_id.clone()),
        };
        self.primary_selected_clip_id = Some(clip_id);
        self.selected_layer_id_for_clip = Some(layer_id);
        self.selection_version += 1;
    }

    /// Toggle a clip in/out of the selection set. Called on Ctrl+Click.
    /// Unity UIState.cs ToggleClipSelection (lines 183-208).
    ///
    /// D1 consequence: this no longer synthesises a region from the clip set
    /// (the old `update_region_from_clip_selection` sync is deleted). A
    /// multi-clip toggle is a pure `Clips` selection — the redundant region
    /// band that used to render alongside the per-clip highlight is gone
    /// (begins the S1 fix; per-clip highlight for the set is unchanged).
    pub fn toggle_clip_selection(&mut self, clip_id: ClipId, layer_id: LayerId) {
        self.clear_layer_selection();
        // Start from the current clip set (empty if the current selection is a
        // region or nothing — a cmd-click while a region is active starts a
        // fresh clip selection, matching the old behaviour where `set_region`
        // had already cleared the clip-id set).
        let mut ids = match &self.selection {
            TimelineSelection::Clips { ids, .. } => ids.clone(),
            _ => HashSet::new(),
        };
        if ids.contains(&clip_id) {
            ids.remove(&clip_id);
            if self.primary_selected_clip_id.as_ref() == Some(&clip_id) {
                // Pick a new primary or None
                self.primary_selected_clip_id = ids.iter().next().cloned();
                self.selected_layer_id_for_clip = None;
            }
        } else {
            ids.insert(clip_id.clone());
            self.primary_selected_clip_id = Some(clip_id);
            self.selected_layer_id_for_clip = Some(layer_id);
        }
        self.selection = if ids.is_empty() {
            TimelineSelection::None
        } else {
            TimelineSelection::Clips {
                ids,
                anchor: self.primary_selected_clip_id.clone(),
            }
        };
        self.selection_version += 1;
    }

    /// Clear all selection (clips, layers, markers, region, and insert cursor).
    /// Unity UIState.cs ClearSelection (lines 211-222).
    pub fn clear_selection(&mut self) {
        self.selection = TimelineSelection::None;
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selected_marker_ids.clear();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selection_version += 1;
    }

    /// Select a set of clips as the whole selection (clears layers, region,
    /// cursor, markers). Used by the duplicate / paste / select-all read-back
    /// paths where the caller computed the id list; the first id becomes the
    /// primary + anchor. Replaces the old "clear then insert into
    /// `selected_clip_ids`" pattern those sites open-coded.
    pub fn select_clips(&mut self, ids: Vec<ClipId>) {
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selected_marker_ids.clear();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        let primary = ids.first().cloned();
        let set: HashSet<ClipId> = ids.into_iter().collect();
        if set.is_empty() {
            self.selection = TimelineSelection::None;
            self.primary_selected_clip_id = None;
            self.selected_layer_id_for_clip = None;
        } else {
            self.selection = TimelineSelection::Clips {
                ids: set,
                anchor: primary.clone(),
            };
            self.primary_selected_clip_id = primary;
            self.selected_layer_id_for_clip = None;
        }
        self.selection_version += 1;
    }

    /// Remove clip ids from the current clip selection (e.g. after deleting
    /// them). No-op unless the current selection is `Clips`. If the primary or
    /// anchor was removed it is repointed at a survivor (or cleared). Mirrors
    /// the old open-coded "`selected_clip_ids.remove` + clear primary if
    /// deleted" loop; deliberately does not bump `selection_version` (the
    /// following structural sync + `prune_stale_references` handles that).
    pub fn deselect_clips(&mut self, remove: &[ClipId]) {
        if let TimelineSelection::Clips { ids, anchor } = &mut self.selection {
            for id in remove {
                ids.remove(id);
            }
            if ids.is_empty() {
                self.selection = TimelineSelection::None;
            } else if let Some(a) = anchor.clone()
                && remove.contains(&a)
            {
                *anchor = ids.iter().next().cloned();
            }
        }
        if let Some(pid) = self.primary_selected_clip_id.clone()
            && remove.contains(&pid)
        {
            self.primary_selected_clip_id = None;
        }
    }

    /// Check if a clip is in the selection set.
    pub fn is_selected(&self, clip_id: &str) -> bool {
        matches!(&self.selection, TimelineSelection::Clips { ids, .. } if ids.contains(clip_id))
    }

    /// The current clip-selection anchor (`None` unless the selection is
    /// `Clips`). D2's shift-click-on-clip gesture reads this to know which
    /// layer/position to extend the range from — the anchor never moves for
    /// that gesture (only `select_clip`/`toggle_clip_selection` pick a new one).
    pub fn clip_selection_anchor(&self) -> Option<ClipId> {
        match &self.selection {
            TimelineSelection::Clips { anchor, .. } => anchor.clone(),
            _ => None,
        }
    }

    /// D2's shift-click clip-range gesture: install an explicit whole-clip id
    /// set as the selection while the anchor stays put. Ableton's anchor is
    /// the fixed end a further shift-click keeps extending from — it never
    /// jumps to the just-clicked clip (unlike `select_clip`/
    /// `toggle_clip_selection`, which each pick a new anchor). `primary`/
    /// `primary_layer_id` become the footer/inspector display target (the
    /// clip just clicked), independent of the anchor. Callers compute `ids`
    /// (the contiguous whole-clip set on the anchor's layer) — this method
    /// only installs the result, matching `select_clips`' division of labor.
    pub fn set_clip_range(
        &mut self,
        ids: HashSet<ClipId>,
        anchor: ClipId,
        primary: ClipId,
        primary_layer_id: LayerId,
    ) {
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selected_marker_ids.clear();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selection = TimelineSelection::Clips {
            ids,
            anchor: Some(anchor),
        };
        self.primary_selected_clip_id = Some(primary);
        self.selected_layer_id_for_clip = Some(primary_layer_id);
        self.selection_version += 1;
    }

    /// Check if a clip is hovered.
    pub fn is_hovered(&self, clip_id: &str) -> bool {
        self.hovered_clip_id.as_deref() == Some(clip_id)
    }

    /// Get a copy of all selected clip IDs (empty unless the selection is `Clips`).
    pub fn get_selected_clip_ids(&self) -> Vec<ClipId> {
        match &self.selection {
            TimelineSelection::Clips { ids, .. } => ids.iter().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// Number of selected clips (0 unless the selection is `Clips`).
    pub fn selection_count(&self) -> usize {
        match &self.selection {
            TimelineSelection::Clips { ids, .. } => ids.len(),
            _ => 0,
        }
    }

    // ── Region Selection ────────────────────────────────────────────

    /// Set a region selection (replaces any clip/layer selection — D1's single
    /// authority: a `TimeRange` cannot coexist with a `Clips` set).
    /// Unity UIState.cs SetRegion (lines 50-68).
    pub fn set_region(
        &mut self,
        start_beat: Beats,
        end_beat: Beats,
        start_layer: i32,
        end_layer: i32,
        layers: &[crate::view::UiLayer],
    ) {
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        // Build the region, then install it as the sole selection.
        let mut region = SelectionRegion::active(start_beat, end_beat);
        let min = start_layer.min(end_layer).max(0) as usize;
        let max = start_layer.max(end_layer).max(0) as usize;
        let upper = max.min(layers.len().saturating_sub(1));
        for layer in &layers[min..=upper] {
            region.selected_layer_ids.insert(layer.layer_id.clone());
        }
        region.start_layer_id = layers.get(min).map(|l| l.layer_id.clone());
        region.end_layer_id = layers.get(max).map(|l| l.layer_id.clone());
        self.selection = TimelineSelection::TimeRange(region);
        self.selection_version += 1;
    }

    /// Clear the region selection. Under D1 a region is the whole selection, so
    /// this returns to `None`; a no-op when the selection isn't a `TimeRange`.
    /// Unity UIState.cs ClearRegion (lines 95-100).
    pub fn clear_region(&mut self) {
        if matches!(self.selection, TimelineSelection::TimeRange(_)) {
            self.selection = TimelineSelection::None;
            self.selection_version += 1;
        }
    }

    /// Whether a region (time-range) selection is active.
    pub fn has_region(&self) -> bool {
        matches!(self.selection, TimelineSelection::TimeRange(_))
    }

    /// The current selection region, if the selection is a `TimeRange`.
    /// Replaces the old always-returns-a-default `get_region()`.
    pub fn current_region(&self) -> Option<&SelectionRegion> {
        match &self.selection {
            TimelineSelection::TimeRange(r) => Some(r),
            _ => None,
        }
    }

    // ── Insert Cursor ───────────────────────────────────────────────

    /// Set insert cursor. Clears EVERYTHING (clips, layers, region) per Ableton behavior.
    /// Unity UIState.cs SetInsertCursor (lines 111-122).
    pub fn set_insert_cursor(&mut self, beat: Beats, layer_id: LayerId) {
        // Skip if nothing would change — same position, no active selection to clear.
        if self.insert_cursor_beat == Some(beat)
            && self.insert_cursor_layer_id.as_ref() == Some(&layer_id)
            && matches!(self.selection, TimelineSelection::None)
            && self.selected_layer_ids.is_empty()
        {
            return;
        }
        self.insert_cursor_beat = Some(beat);
        self.insert_cursor_layer_id = Some(layer_id);
        self.selection = TimelineSelection::None; // cursor replaces clips + region
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear(); // deselect layer headers (Ableton behavior)
        self.primary_selected_layer_id = None;
        self.selection_version += 1;
    }

    /// Move the insert cursor beat without clearing selection (used during playhead scrub).
    /// Unity UIState.cs SetInsertCursorBeat (lines 125-130).
    pub fn set_insert_cursor_beat(&mut self, beat: Beats) {
        self.insert_cursor_beat = Some(beat);
    }

    /// Clear insert cursor if active.
    /// Unity UIState.cs ClearInsertCursor (lines 132-138).
    pub fn clear_insert_cursor(&mut self) {
        if self.insert_cursor_layer_id.is_none() {
            return;
        }
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selection_version += 1;
    }

    /// Whether an insert cursor is placed.
    pub fn has_insert_cursor(&self) -> bool {
        self.insert_cursor_layer_id.is_some()
    }

    // ── Layer Selection ─────────────────────────────────────────────

    /// Select a single layer (clears previous clip, layer, and region selection).
    /// Unity UIState.cs SelectLayer (lines 247-259).
    pub fn select_layer(&mut self, layer_id: LayerId) {
        self.selection = TimelineSelection::None; // clears clips + region
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selected_layer_ids.clear();
        self.selected_layer_ids.insert(layer_id.clone());
        self.primary_selected_layer_id = Some(layer_id);
        self.selection_version += 1;
    }

    /// Toggle a layer in/out of the selection set. Called on Cmd+Click.
    /// Unity UIState.cs ToggleLayerSelection (lines 264-291).
    pub fn toggle_layer_selection(&mut self, layer_id: LayerId) {
        self.selection = TimelineSelection::None; // clears clips + region
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;

        if self.selected_layer_ids.contains(&layer_id) {
            self.selected_layer_ids.remove(&layer_id);
            if self.primary_selected_layer_id.as_ref() == Some(&layer_id) {
                self.primary_selected_layer_id = self.selected_layer_ids.iter().next().cloned();
            }
        } else {
            self.selected_layer_ids.insert(layer_id.clone());
            self.primary_selected_layer_id = Some(layer_id);
        }
        self.selection_version += 1;
    }

    /// Select a range of layers from primary to target (Shift+Click).
    /// Unity UIState.cs SelectLayerRange (lines 297-333).
    pub fn select_layer_range(
        &mut self,
        target_layer_id: &str,
        layers: &[crate::view::UiLayer],
    ) {
        self.selection = TimelineSelection::None; // clears clips + region
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;

        let primary = match &self.primary_selected_layer_id {
            Some(id) => id.clone(),
            None => {
                self.select_layer(LayerId::new(target_layer_id));
                return;
            }
        };

        let anchor_idx = layers.iter().position(|l| l.layer_id == *primary);
        let target_idx = layers.iter().position(|l| l.layer_id == target_layer_id);

        match (anchor_idx, target_idx) {
            (Some(a), Some(t)) => {
                let lo = a.min(t);
                let hi = a.max(t);
                self.selected_layer_ids.clear();
                for layer in &layers[lo..=hi] {
                    self.selected_layer_ids.insert(layer.layer_id.clone());
                }
                // Keep primary as the anchor (first clicked), not the range end
                self.selection_version += 1;
            }
            _ => {
                self.select_layer(LayerId::new(target_layer_id));
            }
        }
    }

    /// Clear layer selection only (preserves clip selection).
    /// Unity UIState.cs ClearLayerSelection (lines 336-340).
    pub fn clear_layer_selection(&mut self) {
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
    }

    /// Check if a layer is explicitly selected (layer selection only).
    pub fn is_layer_selected(&self, layer_id: &str) -> bool {
        self.selected_layer_ids.contains(layer_id)
    }

    /// Unified check: is this layer "active" for ANY reason?
    /// Covers all interaction paths: explicit layer selection, clip selection,
    /// insert cursor placement, and region selection.
    /// Unity UIState.cs IsLayerActive (lines 353-366).
    pub fn is_layer_active(&self, layer_id: &LayerId) -> bool {
        // 1. Explicit layer header selection
        if self.selected_layer_ids.contains(layer_id) {
            return true;
        }
        // 2. Clip selected on this layer
        if self.selected_layer_id_for_clip.as_ref() == Some(layer_id) {
            return true;
        }
        // 3. Insert cursor on this layer
        if self.insert_cursor_layer_id.as_ref() == Some(layer_id) {
            return true;
        }
        // 4. Region selection spans this layer
        if let TimelineSelection::TimeRange(r) = &self.selection
            && r.contains_layer_id(layer_id)
        {
            return true;
        }
        false
    }

    /// Number of selected layers.
    pub fn layer_selection_count(&self) -> usize {
        self.selected_layer_ids.len()
    }

    // Drag/trim lifecycle (begin/end) lives on InteractionOverlay — the single
    // owner of transient gesture state. UIState used to mirror it here.

    // ── Selection Validation ───────────────────────────────────────

    /// Remove clip/layer IDs from selection that no longer exist in the project.
    /// Called after accepting a new project snapshot with a changed data_version.
    /// Returns true if any references were pruned.
    pub fn prune_stale_references(
        &mut self,
        valid_clip_ids: &HashSet<ClipId>,
        valid_layer_ids: &HashSet<LayerId>,
    ) -> bool {
        let mut changed = false;

        // Prune clip IDs (only meaningful when the selection is a clip set)
        if let TimelineSelection::Clips { ids, anchor } = &mut self.selection {
            let before = ids.len();
            ids.retain(|id| valid_clip_ids.contains(id));
            if ids.len() != before {
                changed = true;
            }
            if let Some(a) = anchor.clone()
                && !valid_clip_ids.contains(&a)
            {
                *anchor = ids.iter().next().cloned();
            }
            if ids.is_empty() {
                self.selection = TimelineSelection::None;
            }
        }

        if let Some(ref id) = self.primary_selected_clip_id
            && !valid_clip_ids.contains(id)
        {
            self.primary_selected_clip_id = None;
            self.selected_layer_id_for_clip = None;
            changed = true;
        }

        if let Some(ref id) = self.hovered_clip_id
            && !valid_clip_ids.contains(id)
        {
            self.hovered_clip_id = None;
            changed = true;
        }

        // Prune layer IDs
        let before = self.selected_layer_ids.len();
        self.selected_layer_ids
            .retain(|id| valid_layer_ids.contains(id));
        if self.selected_layer_ids.len() != before {
            changed = true;
        }

        if let Some(ref id) = self.primary_selected_layer_id
            && !valid_layer_ids.contains(id)
        {
            self.primary_selected_layer_id = None;
            changed = true;
        }

        if let Some(ref id) = self.selected_layer_id_for_clip
            && !valid_layer_ids.contains(id)
        {
            self.selected_layer_id_for_clip = None;
            changed = true;
        }

        if let Some(ref id) = self.insert_cursor_layer_id
            && !valid_layer_ids.contains(id)
        {
            self.insert_cursor_layer_id = None;
            changed = true;
        }

        // Prune region layer IDs (only when the selection is a time-range)
        if let TimelineSelection::TimeRange(r) = &mut self.selection {
            let before = r.selected_layer_ids.len();
            r.selected_layer_ids
                .retain(|id| valid_layer_ids.contains(id));
            if r.selected_layer_ids.len() != before {
                changed = true;
            }
            if r.selected_layer_ids.is_empty() {
                self.selection = TimelineSelection::None;
                changed = true;
            }
        }

        if changed {
            self.selection_version += 1;
        }
        changed
    }

    // ── Marker Selection ────────────────────────────────────────────

    /// Select a single marker (clears clips, layers, region, cursor).
    pub fn select_marker(&mut self, marker_id: MarkerId) {
        self.selection = TimelineSelection::None; // clears clips + region
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selected_marker_ids.clear();
        self.selected_marker_ids.insert(marker_id);
        self.selection_version += 1;
    }

    /// Toggle a marker in/out of multi-selection (Shift+Click).
    pub fn toggle_marker_selection(&mut self, marker_id: MarkerId) {
        if self.selected_marker_ids.contains(&marker_id) {
            self.selected_marker_ids.remove(&marker_id);
        } else {
            self.selected_marker_ids.insert(marker_id);
        }
        self.selection_version += 1;
    }

    /// Whether a specific marker is selected.
    pub fn is_marker_selected(&self, marker_id: &MarkerId) -> bool {
        self.selected_marker_ids.contains(marker_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_pin_sets_and_self_clears_on_selection() {
        let mut s = UIState::new();
        assert_eq!(s.pinned_scope(), None);

        // Pinning a scope activates it and preserves selection.
        s.pin_scope(InspectorTab::Master);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Master));

        // Any selection change auto-clears the pin (version moves on).
        s.select_layer(LayerId::new("layer-a"));
        assert_eq!(s.pinned_scope(), None);
        // ...and the layer is genuinely selected (pin didn't disturb it).
        assert_eq!(s.primary_selected_layer_id.as_deref(), Some("layer-a"));
    }

    #[test]
    fn pinning_layer_scope_keeps_the_clip_selected() {
        // The whole point of the generalised pin: viewing a non-Clip rung must
        // NOT drop the clip selection, so the Clip rung stays reachable.
        let mut s = UIState::new();
        s.select_clip(ClipId::new("clip-1"), LayerId::new("layer-a"));

        s.pin_scope(InspectorTab::Layer);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Layer));
        assert_eq!(s.primary_selected_clip_id.as_deref(), Some("clip-1"));

        // Switch to Master, then back to Clip — selection never moved.
        s.pin_scope(InspectorTab::Master);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Master));
        s.pin_scope(InspectorTab::Clip);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Clip));
        assert_eq!(s.primary_selected_clip_id.as_deref(), Some("clip-1"));
    }

    #[test]
    fn clear_scope_pin_releases_without_touching_selection() {
        let mut s = UIState::new();
        s.select_clip(ClipId::new("clip-1"), LayerId::new("layer-a"));
        s.pin_scope(InspectorTab::Master);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Master));

        s.clear_scope_pin();
        assert_eq!(s.pinned_scope(), None);
        // The clip selection is intact — clearing the pin only changes scope.
        assert_eq!(s.primary_selected_clip_id.as_deref(), Some("clip-1"));
    }

    #[test]
    fn pinning_same_scope_twice_is_idempotent() {
        let mut s = UIState::new();
        s.pin_scope(InspectorTab::Master);
        let v = s.selection_version;
        s.pin_scope(InspectorTab::Master); // already pinned — no extra churn
        assert_eq!(s.selection_version, v);
        assert_eq!(s.pinned_scope(), Some(InspectorTab::Master));
    }
}
