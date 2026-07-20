# Drag Surfaces Survey (W2-C, 2026-07-20)

<!-- index: Read-only audit of every drag gesture in the UI — where its hit-test geometry comes from, whether it survives scroll/animation, and whether it's guarded against a mid-gesture content-thread snapshot stomp. Companion to BUG-265 (card drag, fixed W2-B) — this is the sweep of everywhere else. -->

Peter reported drag-and-drop is broken beyond inspector cards (BUG-265). This is the survey
that was ordered up to find out how far the disease spreads. Two independent failure classes
turned up:

1. **BUG-265-CLASS (stale geometry):** hit-test math reads a build-time cached rect/position
   instead of live tree bounds or a live coordinate-mapper conversion — wrong after in-place
   scroll or mid-animation. This was the card-drag bug; **it does not recur anywhere else
   surveyed here.** Every other drag surface either reads `UITree::get_bounds` live or
   recomputes its beat/pixel position fresh from the pointer every frame — the "recompute from
   origin, not incrementally" discipline is applied consistently outside the inspector.
2. **Stomp-vulnerable (the `573b50ea` cluster-C class):** a drag writes its live preview value
   directly into the UI-local `Project` mirror every frame, but the content-thread snapshot
   acceptance path (`app_render.rs` ~line 778-829) has no guard entry for it. A `data_version`
   bump from ANY concurrent edit (not necessarily related to the drag) mid-gesture replaces
   `local_project` wholesale and reverts the in-flight value; the eventual drag-end commit then
   sees old == new and records no undo entry (or, worse, silently discards the user's live
   motion for one frame). This class DOES recur — two confirmed instances below (BUG-280,
   BUG-281), plus a related-but-distinct undo-flood defect (BUG-282) on the same code path.

## Verdict table

| Surface | Geometry source | Scroll-safe? | Anim-safe? | Undo-guarded? | Known symptoms | Verdict |
|---|---|---|---|---|---|---|
| Timeline clip move/trim (`interaction_overlay.rs` `handle_move_drag`/`TrimDrag`) | Live: beat recomputed from pointer each frame via `pixel_to_beat`/`CoordinateMapper`; `viewport/interaction.rs:34-45` explicitly converts pointer Y with `+ self.scroll_y_px()` fresh every hit-test | Yes | Yes (no cached rect at all) | Yes — `DragMode::Move`/`TrimLeft`/`TrimRight` is non-`None`, so `app_render.rs`'s `drag_active` flag (from `self.overlay.drag_mode()`) suppresses ALL snapshot/modulation acceptance for the whole gesture | none on file | HEALTHY |
| Automation point drag (`interaction_overlay.rs` `AutomationDragState`) | Live: re-derives from screen each frame, never incrementally (module comment `interaction_overlay.rs:180-185`) | Yes | Yes | Yes — `DragMode::AutomationPoint` covered by the same `drag_active` suppression | none on file | HEALTHY |
| Automation segment/group/marquee/draw drags (`interaction_overlay.rs:187-308`) | Live: `AutomationGroupDragState`'s doc comment (`:263-266`) explicitly re-resolves `strip_rect` fresh each frame "handles mid-drag scroll" | Yes | Yes | Yes — `DragMode::AutomationSegmentBend`/`AutomationSegmentDrag`/`AutomationMarquee`/`AutomationGroupMove` all covered by `drag_active` (`interaction_overlay.rs:147-172`) | none on file | HEALTHY |
| Marker drag (`ui_bridge/marker.rs`, `panels/viewport/interaction.rs:126-176`) | Live: `self.pixel_to_beat(pos.x)` recomputed every `Drag` event, no cached x | Yes | N/A (no animated geometry) | **No** — `MarkerDragMoved` (`ui_bridge/marker.rs:49-56`) writes `marker.beat` directly into `project.timeline` every frame; `ViewportDrag::MarkerDrag` lives entirely outside `InteractionOverlay`'s `DragMode`, so `app_render.rs`'s `drag_active` check (`self.overlay.drag_mode()`) never sees it, and there is no `ActiveInspectorDrag::Marker` variant to restore the value after a snapshot swap | none on file | **stomp-vulnerable — filed BUG-280** |
| Layer reorder (`ui_bridge/layer.rs`, `panels/layer_header.rs:1419-1463`) | Live: `handle_drag`'s own doc comment (`layer_header.rs:1415-1418`) — queries the same live `CoordinateMapper` the viewport draws lanes from, explicitly "rather than read from a copy" | Yes | N/A | Yes by construction — `LayerDragStarted`/`LayerDragMoved` are no-ops in `ui_bridge/layer.rs:354-356` (`DispatchResult::handled()`); nothing writes to `project` until `LayerDragEnded` computes the whole reorder in one `ReorderLayerCommand` | none on file | HEALTHY |
| Graph-canvas node move (`graph_canvas/interaction.rs` `CanvasDrag::NodeMove`) | Live: `to_graph()` conversion every pointer move (`:490-498`); mutates only the canvas's own local `self.nodes`, not `local_project` | Yes (canvas has its own pan/zoom, read live) | N/A | Yes by construction — `MoveGraphNode` is emitted exactly once, on release (`:1307-1313`); nothing commits during the drag | none on file | HEALTHY |
| Graph-canvas wire drag (`CanvasDrag::WireFrom`) | Live: cursor position only, no project write until release | Yes | N/A | Yes by construction — one `ConnectPorts` on release, nothing during | none on file | HEALTHY |
| Graph-canvas param-scrub, BOUND row (`CanvasDrag::ParamScrub`/`VecScrub`, bound branch, `app_render.rs:2906-2971`) | Live: `delta_px` off `session.start.x`, recomputed every move | Yes | N/A | **No** — a `BoundNodeParamDrag` session (`self.bound_node_param_drag`) is opened and live-written into `self.local_project` every tick (`app_render.rs:2955-2957`), but the snapshot-acceptance restore block (`app_render.rs:823-827`, `878-882`) only ever calls `self.active_inspector_drag.apply(...)` — `bound_node_param_drag` is never consulted there, and the graph canvas has no `drag_active`-equivalent suppression | none on file | **stomp-vulnerable — filed BUG-281** |
| Graph-canvas param-scrub, UNBOUND row (same `CanvasDrag::ParamScrub`/`VecScrub`, unbound branch, `app_render.rs:2972-2980`) | (geometry n/a — this is an undo-batching defect, not a geometry one) | — | — | **No** — every `on_pointer_move` tick pushes `GraphEditCommand::SetGraphNodeParam` (`graph_canvas/interaction.rs:504-539`, `:541-569`), and the unbound arm in `app_render.rs` executes a brand-new `SetGraphNodeParamCommand` as a full undo-worthy `Execute` on EVERY tick — confirmed by the release-handler's own comment (`graph_canvas/interaction.rs:1315-1323`): "the scrub emitted its value on each pointer move; nothing to finalize for an ordinary row" (`EndGraphNodeParamScrub` only closes out the bound case). No other drag family in the codebase behaves this way — all others batch to one undo entry at drag-end | none on file | **undo-flood — filed BUG-282** |
| Browser → timeline drag-in, Finder files (`drag_interpose.rs`, `drag_hover.rs`) | Live where the platform interposition installs (`drag_interpose::drag_position()`, polled fresh via `draggingUpdated:`); falls back to last-known `cursor_pos` otherwise | N/A (one-shot resolve on drop) | N/A | N/A — single atomic action on `DroppedFile`, no per-frame project write | already tracked: the file's own doc comment flags the AppKit-forwarding assumption as unverifiable headless, see `docs/TIMELINE_INGEST_DESIGN.md` P1 gate | HEALTHY (design already accounts for its one open unknown; not a new finding) |
| Browser → timeline drag-in, internal (in-app asset panel to timeline) | — | — | — | — | no such mechanism exists — `graph_palette.rs` and `browser_popup.rs` are click-to-add only, no drag path found | N/A — surface doesn't exist |
| Slider / param scrub family (`slider.rs`) | Live: `update_value` reads `tree.get_bounds(ids.track)` (`slider.rs:470`) | Yes | Yes | N/A (base widget; drag-guard is the caller's responsibility — see mapping-range/card-slider rows) | already fixed 2026-07-19 (BUG-258/259) — this IS the reference pattern BUG-265's fix in W2-B replicates | HEALTHY |
| Text-selection drag (`text_input.rs` `drag_to`) | Byte offset computed by the caller from a live pixel-to-char mapping each call | Yes | N/A | N/A — local edit-buffer state only, never touches `Project` | none on file | HEALTHY |
| Scene outliner reorder | — | — | — | — | no such surface — `scene_setup_panel.rs`'s "outliner" is click-select only, no drag/reorder mechanism | N/A — surface doesn't exist |
| Card drag (baseline reference) | Was build-time `card_y()` snapshot | No (pre-fix) | No (pre-fix) | N/A | BUG-265 | BUG-265-CLASS — **fixed in W2-B** (concurrent lane, not this survey's scope) |
| Scrollbar drags (`scroll_container.rs` `drag_to_scroll`) | Live: `tree.get_bounds(thumb_id)` (`:195`) | Yes | Yes | N/A — scroll position isn't undo-tracked | none on file | HEALTHY |
| Mapping-range/affine drags (healthy reference, `ActiveInspectorDrag::MappingRange`/`MappingAffine`) | Live, same `pending_actions` loop as graph-canvas param-scrub | Yes | N/A | Yes — fixed BUG-262, this is the pattern the graph-canvas bound-row case (BUG-281) should have followed and didn't | none open (BUG-262 fixed) | HEALTHY |

## Findings not captured as new bugs

- The completeness sweep (`rg -i "drag" crates/manifold-ui crates/manifold-app --files-with-matches`)
  surfaced no additional gesture surface beyond the named set — everything else the sweep
  matched was either a doc comment, a struct/field named `*_drag*` already covered above, or
  infrastructure (`cursors.rs`, `input.rs`, `intent.rs`) that routes events to the surfaces
  already audited rather than owning geometry itself.
- The graph-canvas param-scrub findings (BUG-281, BUG-282) share one root: the `pending_actions`
  loop in `app_render.rs` is the same mechanism BUG-262/BUG-263 already flagged as
  under-tested and partially unguarded for the mapping-sidebar case. The bound-row branch added
  since BUG-262 (`BoundNodeParamDrag`, D1 of `PARAM_TWO_WAY_BINDING_DESIGN.md`) repeats the
  same unguarded-live-write shape on a fresh field instead of extending `ActiveInspectorDrag`
  or the mapping fix's pattern — this survey doesn't judge whether that's an omission or a
  scoping call, only that the resulting behavior is stomp-vulnerable by the same test BUG-262
  used.

## New backlog entries filed

BUG-280, BUG-281, BUG-282 (high-water mark: BUG-282; range 283-289 unused, none needed).
Full write-ups in `docs/BUG_BACKLOG.md`.
