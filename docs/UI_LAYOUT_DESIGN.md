# UI Layout Design — full-width timeline + right-side tabbed inspector

A UI/UX layout redesign that sits **on top of** the finished UI Architecture Overhaul — it is design work, not architecture work (the substrate it uses, `ScreenLayout` + the declarative View/Panel API, already exists). The goal: make the timeline the uninterrupted full-width spine of the editor, with the preview and a single tabbed inspector stacked above it. The inspector mirrors the current selection — one source of truth, no parallel tab state.

## Why

- **Timeline is root.** MANIFOLD is "Ableton + Max for Live, for live video": the arrangement is the thing you compose *in*, so it should own the horizontal axis. Today the timeline is boxed into a lower-right quadrant, reading like a panel rather than the spine.
- **Right-side inspector is the convention** our users already live in — DaVinci Resolve, TouchDesigner, Blender, Unreal/Unity, Figma all put the selected-thing inspector on the right. The exceptions (Ableton, Resolume) are timeline tools that push detail to the *bottom*; MANIFOLD sits on that seam (timeline-tool lineage **and** DCC-style preview/graph/effect-stack). Right inspector + bottom timeline resolves it.
- **Preview top-left** (not centered) falls naturally out of the two-region top band, and matches Resolve/Premiere (program monitor top-left, inspector right, timeline bottom).

## Target layout

```
┌──────────────────────────────────────────────────────────┐
│  TRANSPORT BAR                                  full width │
├────────────────────────────────────┬─────────────────────┤
│  PREVIEW                            │  INSPECTOR           │
│  (top-left quad)                    │  [Clip|Layer|Grp|Mas]│
│                                     │  ─ tab strip ─       │
│                                     │  params for active   │
│                                     │  scope only          │
├────────────────────────────────────┴─────────────────────┤
│  TIMELINE                                       full width │
└──────────────────────────────────────────────────────────┘
       ▲ split_ratio (drag ↕)              ‖ inspector_width (drag ↔)
```

Three resizable regions, all already persisted (`inspector_width`, `timeline_split_ratio`):
- **Transport bar** — full width, top (unchanged).
- **Top region** — below transport, above timeline, full width. Splits horizontally into **preview (left)** and **inspector (right)**.
- **Timeline** — full width, bottom. The spine.

## The tabbed inspector

One inspector, **tabbed and selection-driven**. The tabs are the **hierarchy of what's currently selected**, local→global:

```
Clip · Layer · Group · Master
```

Rules:
- **One source of truth.** Tab and selection always mirror. Selecting a clip moves the tab to it; clicking a tab re-points the selection up/down the ownership chain. They never drift.
- **Contextual tabs** — show only the rungs that exist. `Group` appears only when the selected layer has a `Group` ancestor (`LayerType::Group` + `parent_layer_id`, both already in core). Nothing selected → `Master` only.
- **Master is a tab, not a fake timeline lane.** It has no clips and no time extent, so a lane would be an orphan. Master is the always-present right-most rung; it parks the inspector until the next selection.

This replaces today's side-by-side **Master | Layer** two-column inspector (`InspectorCompositePanel`) with a single column whose active scope follows selection.

## Implementation — two phases

Each phase ships working on its own.

### Phase A — Layout reshape  ✅ DONE (2026-06-23)

Pure geometry, no behavior change. All in the `ScreenLayout` single source of truth ([crates/manifold-ui/src/layout.rs](../crates/manifold-ui/src/layout.rs)) plus the resize-handle in [crates/manifold-app/src/ui_root.rs](../crates/manifold-app/src/ui_root.rs):

- `content_area()` → full width (was offset right of the left-side inspector).
- New `top_region()` → below transport, above timeline, full width.
- `video_area()` (preview) → top-**left** of the top region, left of the inspector.
- `inspector()` → top-**right** of the top region (was full-height on the left).
- `timeline_area()` / `header` / `footer` / `timeline_body` / `layer_controls` → now full width automatically (they derive from `content_area`).
- `content_left()` → drops the inspector width (inspector no longer pushes content from the left).
- **Inspector resize handle** moved to the inspector's **left** edge, hit-test y-bounded to the (now shorter) inspector, and the drag sign flipped (`start_width - delta`) so dragging left widens it.
- Layout unit tests rewritten to the new geometry.

The two-column `InspectorCompositePanel` relocates to the top-right as-is (it reads `layout.inspector()`), just shorter — its scroll containers absorb the reduced height. Phase B then collapses the two columns into the tabbed single column.

### Phase B — Tabbed inspector + Group + mirror  ✅ DONE (2026-06-23)

1. **One `InspectorScope` as the single source of truth**, derived from selection: clip → `Clip`; else layer → `Layer`; else `Master`. The only new state is a small `master_scope_active: bool` on the existing `UISelectionState` (set when the Master tab is clicked, cleared on any clip/layer selection). The active tab is otherwise pure-derived — this is what makes "always mirror" true with one authority.
2. **Tab click = navigate the hierarchy** (changes selection, never forks from it): Layer tab → select the clip's layer; Group tab → select the parent `Group` layer; Clip tab → select the clip; Master → set the flag, deselect.
3. **Contextual tab set** computed from the selected item's `parent_layer_id` ancestry.
4. **Render a tab strip** in the inspector View tree and build **only the active scope's** content (today both Master and Layer build simultaneously, [crates/manifold-ui/src/panels/inspector.rs](../crates/manifold-ui/src/panels/inspector.rs)). Add `Group` to the `InspectorTab` enum and route it to the selected group layer.
5. **Reuse what exists** — the `SelectInspectorTab` action, per-tab effect routing, and the selection→tab derive fn are already built; this phase tightens them into one bidirectional binding and deletes any now-redundant independent tab state.

**Settled decision:** Master is kept in the selection-driven model via a Master *pin*, not a fake lane. Implemented as `master_pinned_at_version: Option<u64>` on `UIState`: Master is active iff the stored version still equals `selection_version`, so any selection change (which already bumps the version) auto-clears the pin — zero edits to the ~10 selection mutators. `clear_master_scope()` releases the pin without touching the timeline selection.

**Behavior note (strict mirror):** a tab click changes the *selection*. Clicking `Layer` while a clip is selected selects the layer (the clip deselects), so the `Clip` rung then disappears — the tab set always reflects the live selection's ancestry. This is the single-source-of-truth tradeoff; if A/B-ing a clip against its layer turns out to want the Clip rung to persist, revisit by making the tab a view-level into a stable selection context instead of a selection change.

**Where it landed:**
- `InspectorTab` gains `Group`; `is_layer_scope()` folds Group into the layer column everywhere.
- `InspectorCompositePanel`: `active_tab` + `available_tabs` + `configure_tabs`/`set_active_tab` drive the existing `*_visible` gates so only the active scope renders; `build_tab_strip` renders the rungs; tab clicks route through `route_click` → `SelectInspectorTab`. The two scroll columns still build every frame (inactive one collapses to zero width) so no node ids go stale.
- `sync_inspector_data` computes the available rungs + active scope from the selection (+ pin) and the layer hierarchy, then `configure_tabs`.
- `inspector_select_tab` (in `ui_bridge`) handles the click: Master pins, Clip releases the pin, Layer/Group re-point the selection (and `active_layer`) up the chain.

## Constraints honored

- No new shared state (the one flag lives in existing `UISelectionState`); no `Arc<Mutex>`.
- Entirely within the existing declarative View/Panel API and the `ScreenLayout` SSOT — no hacking, no forked layout paths.
