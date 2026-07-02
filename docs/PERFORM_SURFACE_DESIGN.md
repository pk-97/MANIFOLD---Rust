# Perform Surface & UI Widget System

**Status: APPROVED design, not built · 2026-07-03 · Fable queue (perform surface builder, steal-pass S2)**
**Prerequisites: none for P1. P2 (session perform) requires `docs/SESSION_MODE_DESIGN.md` to be built.
P4 (editor workspaces) gets its own design pass when scheduled — this doc only pins its direction.**

Peter's scope (2026-07-03): **"let's just keep it simple for now, doesn't need to be complex at
all to start"** — plus two extensions: **"the timeline mode should also reuse the UI widget
system"** and **"it would be nice to allow users to custom build and modify their editor and
workflow setups"** (the workspaces layer, §8).

---

## 1. What exists (audited 2026-07-03)

- **Perform mode is draw-only.** `crates/manifold-app/src/perform_mode/` — `render.rs` (942
  lines) hand-paints sync indicators, the cue HUD, and the macros column straight through
  `UIRenderer`; the only interactive element is the exit button (one cached hit-test rect in
  `state.rs`). No UI tree, no chrome, no input system.
- **The view-model snapshots already exist.** `macros.rs::MacroDisplay::snapshot`, `cue.rs`,
  `tracks.rs` — perform data is already pulled into plain display structs before drawing.
  Widgetization reuses these; it does not invent new data plumbing.
- **The chrome API is the substrate.** `crates/manifold-ui/src/chrome/` — declarative `View`
  trees, mini-flexbox `layout::solve`, `ChromeHost` reconciler (build / in-place update),
  `ViewIntent` → `PanelAction`, `Theme` inheritance. See `docs/CHROME_API_DESIGN.md`.
- **Cards are already the live surface.** `ParamCardPanel` with `CardContext::Perform`
  (`panels/param_card.rs`) binds by identity and drives `param_values` — the invariant in
  `feedback_param_values_is_performance_surface` is untouched by this design.
- **Layering is settled.** `manifold-ui` depends only on `manifold-foundation`; all core↔UI
  conversion lives in `manifold-app/src/ui_translate.rs`. Widgets follow the same rule.
- **Perform-mode safety rules** (`perform_mode/mod.rs` header): content thread never touched,
  triple-redundant exit, quiesce on entry, full rebuild on exit. All preserved verbatim.

## 2. Decisions

- **D1 — Two perform modes, entry follows context.** Timeline perform (today's HUD content:
  sync, cues, macros) and session perform (the session grid at stage scale + the same strip).
  One Perform button: performing an arrangement → timeline perform; in session view → session
  perform. No setting. (Peter: "will likely have a perform mode for timeline shows … and a
  perform mode for the session mode.")
- **D2 — Perform mode becomes chrome-hosted.** A real UI tree via `ChromeHost` in the main
  window, input through the existing `window_input.rs` path. The hand-drawn path is deleted in
  the same phase — no parallel render path survives (root fix; see
  `feedback_no_silent_fallbacks_or_interim_stopgaps`).
- **D3 — A widget is a chrome panel fed by a view-model.** View-model in (translated in
  `ui_translate.rs`), `View` tree out, gestures out as intents. Escape hatch for visuals the
  flexbox can't express (beat-flash, meters): a painter-style custom-draw widget — precedent
  is the Phase 8 graph-canvas `Painter` trait. Widgets never import `manifold-core`.
- **D4 — Surfaces are data.** A surface def is a serde value: widget kind + grid cell per
  widget. **Default layouts ship as bundled defs, not code** — the same pattern as bundled
  effect/generator presets. v1 has no builder UI and no per-project persistence; the bundled
  defaults are the only surfaces. The data path is still proven day one, because the default
  surface *is* a def being instantiated.
- **D5 — Coarse grid.** Cells on a 12×8 grid, widgets span whole cells. No freeform pixels,
  no docking, no overlap. Push-style.
- **D6 — v1 widget set = exactly today's HUD, wrapped:** SyncStatus, CueHud, MacroColumn,
  ExitButton — plus SessionGrid in P2. Deferred (§7): param cards on the surface, stage-scale
  knob/fader/pad, XY pad, next-clip preview (S3), output viewport, audio meters, pages.
- **D7 — Bindings address existing stable identities only** (`param_id` slots, macro index,
  `LayerId`, scene index). Widgets are a presentation layer over the same slots MIDI/OSC/
  Ableton already write. Nothing new in the binding model; the B++ card unification
  (`docs/CARD_TARGET_UNIFICATION.md`) is untouched and unblocked.
- **D8 — R1 revised, precisely.** The steal-pass R1 rejection of the VDMX *DIY UI toolkit*
  stands. What changes: **configurable arrangements of purpose-built panels** (Ableton/
  Blender-style workspaces) are now in scope as a future layer. Fixed default editor stays the
  product position; arranging is opt-in. (Peter, 2026-07-03: "allow users to custom build and
  modify their editor and workflow setups.")

## 3. Data model (v1)

```rust
// manifold-ui (foundation-only types), serde
pub struct SurfaceDef {
    pub id: String,                  // "timeline-perform-default"
    pub widgets: Vec<WidgetInstance>,
}
pub struct WidgetInstance {
    pub kind: WidgetKind,            // registry key
    pub cell: GridRect,              // col/row/col_span/row_span on the 12×8 grid
    // v1: no target field — the four HUD widgets know their data.
    // The builder phase adds `target: Option<WidgetTarget>` (param slot, macro,
    // layer, scene) when user-placed widgets need it. Serde-default keeps defs stable.
}
pub enum WidgetKind { SyncStatus, CueHud, MacroColumn, ExitButton, SessionGrid }
```

- **Registry:** one descriptor per `WidgetKind` — display name, min/max span, whether it needs
  a target. Same shape as the primitive registry / spec sheets; this is the §10.2
  "widget catalog" deferred item landing with its first real customer. Because surfaces are
  data with a catalog, they become MCP/agent-authorable for free once the builder exists.
- **Grid solve is trivial:** cell → rect math inside the surface panel; chrome flexbox lays
  out *inside* each widget. No new layout engine.

## 4. Hosting & input

- One `PerformSurfacePanel` (implements the existing `Panel` trait, `panels/mod.rs:777`)
  owns the tree: reads the active `SurfaceDef`, instantiates widgets from the registry,
  builds their `View` trees into grid rects, reconciles via `ChromeHost` per frame.
- Input rides the normal path: `ViewIntent` → intent registry → `PanelAction`, exactly like
  chrome panels in the editor. Perform-mode gating (`input.rs`) keeps filtering what it
  filters today; the triple-exit ladder is unchanged (exit button becomes a widget but its
  three redundant detection paths stay).
- Session-grid gestures are **`ContentCommand` launches, not undoable `Command`s** — per
  SESSION_MODE_DESIGN §"launch semantics" (performance gestures, like MIDI triggers). Surface
  *editing* (builder phase) is the opposite: undoable `EditingService` commands.

## 5. What this deliberately is not (v1)

No builder UI. No pages. No XY pads or stage-scale control chrome. No per-project surface
persistence. No output/preview viewport. No editor workspaces. No touch/tablet story. Each is
listed in §7 with its trigger; none of them changes the v1 data model except additively.

## 6. Theming

Widgets take a `Theme` (`chrome/theme.rs`) from the surface panel; source-identity accents
(Ableton purple, LFO magenta, audio green, trigger orange) flow down exactly as the modulation
drawer does today. Zero new styling infra.

## 7. Deferred layers, in order

1. **Builder v1** — arrange/resize/add/remove widgets on the grid, per-project persistence,
   undoable commands, `WidgetTarget` for param/macro/layer widgets. Trigger: after session
   perform has proven the widget set.
2. **Stage-scale control chrome** — big knob / fader / trigger pad, XY pad (two-param). These
   are new *controls*, not new arrangement; design them against real stage use.
3. **Cue/preview + output widgets** — steal-pass S3; needs the low-res preview context.
4. **Pages** — `Vec<SurfaceDef>` + active index, MIDI-bindable switch.
5. **Editor workspaces** — dock/split container for the editor's purpose-built panels, saved
   workspace presets. Reuses the registry + def persistence; the container is genuinely new
   work (it does *not* fall out of the grid). Own design doc when scheduled. The DIY-toolkit
   rejection (steal-pass R1) still bounds it: arranging purpose-built panels, never composing
   arbitrary UI.

## 8. Phasing (Sonnet-executable)

- **P1 — Widget substrate + timeline perform migration.** `SurfaceDef`/`WidgetKind`/registry;
  `PerformSurfacePanel`; wrap sync/cue/macros/exit as widgets (reusing the existing snapshot
  structs); bundled `timeline-perform-default` def; **delete the hand-drawn path in
  `render.rs` in the same phase.** Gate: headless PNG comparison against the current HUD
  (visual parity within layout tolerance — see `reference_ui_headless_png_verification`),
  all three exit paths verified, focused `manifold-ui` tests. This is UI-infrastructure:
  full workspace sweep before merge.
- **P2 — Session perform.** Ships with the session-mode build: SessionGrid widget (launch
  gestures → `ContentCommand`), `session-perform-default` def, Perform-button context routing
  (D1). Gate: launch quantization behavior matches session-mode spec; grid readable at stage
  distance.
- **P3 — Builder v1** (deferred item 1 above) — only after P2 has been performed with.
- **P4 — Editor workspaces** — separate design doc first (§7.5).

## 9. Decided — do not reopen

1. Two perform modes; entry follows context; one Perform button.
2. Perform mode is chrome-hosted; the hand-drawn path is deleted in P1, not kept beside.
3. Widgets = chrome panels fed by view-models; painter escape hatch for custom visuals;
   foundation-boundary rules apply (`ui_translate.rs` is the only converter).
4. Surfaces are serde defs; defaults ship as bundled defs, not code.
5. Coarse 12×8 grid, whole-cell spans; no freeform, no docking on the perform surface.
6. v1 = today's HUD wrapped + session grid; everything else is §7 deferred, additive-only.
7. Widget bindings address existing stable identities; `param_values` stays the live surface.
8. R1 split: DIY UI toolkit stays rejected; workspace arrangement of purpose-built panels is
   the future layer (§7.5), fixed editor remains the default.
9. Launches are `ContentCommand` gestures (never undoable); surface edits are undoable
   `EditingService` commands (builder phase).
