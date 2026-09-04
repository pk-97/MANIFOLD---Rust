# Preset Browser — live audition grid, crud removal, and layout polish

**Status:** PROPOSED — awaiting adversarial review and Peter approval · 2026-09-04 · k3 (lead)
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

The effect/generator browser is a wall of identical gradient thumbnails on enormous
cards — the thumbnail input is a synthetic test gradient (parity-harness input,
`preset_thumbnail.rs:252-258`), so every effect preview shows the same magenta-green
wash and carries zero choosing information. Peter's directives:

- "Let's preview ALL of them at once so you can see what effects do what in real time
  as a ease of choice type thing."
- "The preview should correspond to the relevant Inspector type, clip, layer, or
  master."
- "There's also a lot of test 'crud' in the picker that was never cleaned up and it
  makes it hard to navigate quickly."
- "Some of the text isn't padded well or is hard up against edges or not aligned
  nicely or fits inside elements well. Just doesn't look polished."

Companion docs: `docs/archive/PRESET_LIBRARY_DESIGN.md` (shipped library design whose
P5/P6 browser decisions this redesign supersedes on the cell-rendering front only) ·
`docs/EFFECT_RUNTIME_UNIFICATION.md` + `docs/EFFECT_CHAIN_LIFECYCLE.md` (the runtime
the audition pool builds on) · `docs/MANIFOLD_GPU_ARCHITECTURE.md` (hot-path rules).

Binding constraints: **hot path** (audition renders ride the content thread's per-frame
work; round-robin + budget back-off are the design, not optimizations) · **thread
residency** (audition never mutates `Project`; the existing two-thread model is
untouched) · **undo** (audition is read-only; only the commit click rides
`AddEffectCommand`, which is already undoable).

## 1. Audit — what exists (verified 2026-09-04)

Verified by three read-only lane audits (picker UI code, engine preview path, preset
content), lead-reviewed. <!-- sections 1.1-1.3 filled from audit reports -->

### 1.1 Engine preview path

- Preset → instance: browser click is pure data — `AddEffectCommand{target,
  PresetInstance, insert_index}` (`crates/manifold-editing/src/commands/effects.rs:28`)
  inserts a `PresetInstance` clone; GPU work is lazy, next frame, per topology hash.
  Standalone build without any Project/EditingService:
  `PresetRuntime::from_def_with_device` (used at
  `crates/manifold-renderer/src/preset_thumbnail.rs:146`).
- Pipelines are shader-hash cached and **resolution-independent**
  (`crates/manifold-gpu/src/metal/device.rs:127`) — preview-size cells share pipelines
  with live chains. Cold cost is one `ChainConstruction` + possible `PipelineCompile`
  per cell at open (counted by `cold_touch.rs:52-79`).
- Compositor taps (all read-only `&GpuTexture`):
  pre-chain `layer_bufs[layer_id].source_texture()` (`layer_compositor.rs:1948`),
  post-chain `PresetRuntime::output_texture()` + `layer_outputs_scratch` (:1789),
  master `self.main` (:2020-2028). Per-frame capture precedent:
  `preview_request` / `preview_texture()` (:2622/:2629).
- Texture transport: content and UI hold separate MTLDevices
  (`crates/manifold-app/src/shared_texture.rs:1-8`); live frames cross via
  triple-buffered Rgba16Float IOSurface `SharedTextureBridge`; UI samples via
  `UIRenderer::register_external_texture` (`ui_renderer.rs:744`).
  **The graph editor already ships an N-cell live atlas**: content packs every
  visible node's output into one atlas (`content_pipeline.rs:2974-2999`), publishes
  one IOSurface, UI samples per-cell UVs (`editor_bridge.rs:1580-1623`,
  `graph_canvas::set_node_preview_src`). Structurally identical to an audition grid.
- Missing: round-robin cell scheduler (nothing throttles preview work across frames);
  a budget-backoff signal (`frame_wall_ms` exists at `content_thread.rs:964` but is
  not exposed to the pipeline — a few lines to thread it in).

Classification: instantiation **exists** · taps **exist** · transport **exists** ·
scheduler **genuinely new** (small) · budget signal **one wire away**.

### 1.2 Picker UI (browser_popup.rs, picker_core.rs, invocation sites)

Verified with headless renders (label/baseline defects confirmed visually).

- Invocation: three real open paths in `crates/manifold-app/src/ui_root/dropdowns.rs`:
  Inspector Master "+ Add Effect" (:352-385), Inspector Layer "+ Add Effect" (same
  arm; rendered for **every** layer type with no DMX gate,
  `manifold-ui/src/panels/inspector/render.rs:568`), generator card type row
  (:386-409, carries `layer_id`). No clip-slot invocation exists. A second
  `BrowserPopupPanel` instance in Node mode lives in the graph editor
  (`app_render.rs:1957-2050`). **Context filtering is essentially none** — only
  `PresetKind` at :358/:393.
- **F1 (blocks redesign): effect adds target the *active* layer, not the invoking
  context.** The request carries `layer_id: None` (dropdowns.rs:377) and dispatch
  re-resolves the active layer at pick time (`ui_bridge/dispatch/params.rs:781-792`).
  Modal popup makes this safe by accident; the target-tapped audition grid breaks
  it — the audition source and the add target must both be the invoking context.
- **F2: badge and `source` are two fields for one concept** — every Factory cell is
  badged "Factory" (zero information; excluded from right-click menu by design,
  browser_popup.rs:890-897).
- **F3: whole layout is fixed constants** — `POPUP_WIDTH 600`, `CELL_WIDTH 185` ×
  `HEIGHT 42.5`, 3 derived columns, `POPUP_MAX_HEIGHT 550` (browser_popup.rs:32-44).
  Width never sizes to content or screen.
- **F4: Effect vs Generator browsers diverge by construction** — Generator mode
  passes `category_names: vec![]` (dropdowns.rs:403) though generator presets have
  categories (deliberately dropped, :111-112): generators are one flat list.
- **F5: search auto-focus asymmetry** — Node picker auto-focuses
  (app_render.rs:2036-2049), main-window popup does not; effect items have
  `search_text: None` (dropdowns.rs:154) so aliases are Node-mode-only.
- **F6: keyboard nav is list-shaped in a grid** — linear move in a 3-column grid
  (Down jumps diagonally), never `scroll_to_reveal` (scroll_container.rs:279 exists,
  unused), no Left/Right/Home/PageUp.
- **F7-F13 (UX defects):** caption strip (bottom 14px of 42.5px cell,
  browser_popup.rs:667-677) does not back the vertically-centered label (~21px) —
  the label floats raw over the thumbnail; badge pinned at `CELL_HEIGHT - 13`
  (:718-733) with a different baseline than the label; no empty state (0 results =
  blank collapsed popup); labels space-padded (`prefix = "     "`, :687) instead of
  x-inset; stale accent-color table (:1001-1009) — "Post-Process"/"Surveillance"
  colors for a registry that has Spatial/Color/Stylize/Filmic/Diagnostic
  (`manifold-core/src/preset_type_registry.rs:46`), Color/Stylize/Diagnostic render
  grey; chip width is a byte-length heuristic (:997-999), overflows silently past
  ~7 categories; wheel scroll not hit-tested (:1111-1114).
- **F14-F17 (cleanup):** dead "Generators" category skip (:568-570, :797); dead
  no-op `update()`/`is_animating()` (:261/:270); per-click `Vec<String>` clones in
  `handle_click` (:788-791); dead `InspectorTab::Clip` arm in AddEffect dispatch
  (params.rs:797-800).
- Add path: click → `BrowserPopupAction::Selected` → `PanelAction::Params(AddEffect)`
  / `ProjectAction::SetGenType` → `AddEffectCommand` / `ChangeGeneratorTypeCommand`
  (preserves params/drivers/envelopes) executed on the UI mirror + mirrored via
  `ContentCommand::Execute` → EditingService → undoable. Undo does not reopen the
  browser; owned search session cancelled by the closed-overlay pump. Dismissal:
  backdrop, Escape, selection, paste, perform-mode entry.

Classification: layout/UX defects **exist-and-replace** (F3, F7-F13) · context/target
bug **blocks redesign** (F1) · badge/source and category-divergence **decide in this
design** (F2, F4) · cleanup **trivial** (F14-F17).

Classification: layout/UX defects **exist-and-replace** (F3, F7-F13) · context/target
bug **blocks redesign** (F1) · badge/source and category-divergence **decide in this
design** (F2, F4) · cleanup **trivial** (F14-F17).

### 1.3 Preset content (factory presets, thumbnails, obligations)

- **16 of 47 committed thumbnails are byte-identical placeholders** — 10 effects
  share one UV-gradient PNG (identical MD5, zero effect applied), 4 are pure black
  (BlobTracking, StylizedFeedback, MriVolume, OilyFluid). Batch rendered
  2026-07-05/10, never regenerated; render code and presets changed under it.
  ~14/27 effect thumbnails misrepresent the effect.
- **22/42 generator thumbnails missing** (all 11 LED-*, plus ApricotWeather,
  BlossomWire, Caustics, Cymatics, FogBlast, Lantern, LightOrbit, Lightning,
  SceneStarter, SceneStrobe, Skin, TimeScrub). Fallback is graceful by design
  (`thumbnail: Option`, browser_popup.rs:661-664) — text cell, never browse-time
  render.
- **No layer-type gating for generators** — a DMX layer can pick Black Hole; a video
  layer sees all 11 LED-* patterns (`build_preset_picker_items`,
  dropdowns.rs:113-194). LED presets are referenced only by tests.
- **Test presets ship in the bundle**: NodeGraphTest.json (already `available=false`,
  but JSON + 35KB placeholder PNG ship; referenced by app_render.rs/app.rs and a
  param-storage migration — move to test fixtures) and TrivialPassthrough.json (no
  `presetMetadata`, invisible in the browser, self-described smoke test).
- **Scene-asset generators render black bare** — TimeScrub, Skin (need a GLB
  animation/mesh source), MriVolume (needs `node.image_folder` media). A browsing
  user sees looks that produce nothing.
- **Miscategorization table verified** (current buckets: Spatial 6 / Color 4 /
  Stylize 7 / Filmic 6 / Diagnostic 3): FilmGrain under Stylize while
  ChromaticAberration sits in Filmic; Glitch/DigitalDrift (digital corruption) in
  Filmic; Infrared (false-color emulation) in Color; AutoGain (level utility) in
  Stylize; VoronoiPrism (geometric warp) in Stylize.
- `assets/reference-presets/` (9 graph-draft fixtures) never reach the browser —
  not crud, flagged so the redesign doesn't mistake them for shipped content.
- Obligations from `docs/archive/PRESET_LIBRARY_DESIGN.md` (SHIPPED P0–P6): **D1**
  tracking-until-first-definition-edit (never reopen) · **D3** modified =
  `graph.is_some()` · **D4/D5** three storage tiers + snapshot-on-save · **D6**
  "I like the popup" — the browser stays an insert popup, source row is a filter,
  management is right-click in place, no separate library window · **D7**
  save-time-only thumbnails — *superseded by this design* for the browser (static
  PNGs retire from browser cells; user-preset save-time renders stay for other
  consumers e.g. file pickers) · **D8** `effect_type` based-on id · **D9** imports
  mint EmbeddedPresets. Category taxonomy was never pinned — recuration is a
  product call, licensed.
- Other crud: preset `description` fields carry ~82KB of engineering essays nothing
  reads (strip from shipping assets) · preset search matches label only (alias
  machinery exists for graph atoms, never extended) · SceneStrobe generator
  displays as "Strobe", colliding with the effect in search.

## 2. Decisions

- **D1 — The browser becomes a live audition grid; static thumbnails retire from
  browser cells.** Every visible cell shows the preset actually rendering, applied
  to the frame at the browser's invocation point. Peter: "preview ALL of them at
  once so you can see what effects do what in real time as a ease of choice type
  thing." *Rejected: regenerate static PNGs over a photographic reference frame* —
  fixes the identical-gradient look but stays a picture of someone else's frame; a
  live rig can audition on the actual show. *Rejected: hover-only audition* — Peter
  chose all-at-once.
- **D2 — Audition input = the invoking context's tap point.** Master "+ Add Effect"
  auditions against `layer_compositor.rs:2020` (`self.main` pre-master-chain); layer
  "+ Add Effect" against that layer's pre-chain `layer_bufs[..].source_texture()`
  (:1948); a future clip-slot entry against the clip's source texture. This fixes
  audit finding F1 as a precondition: the invocation context (layer id, tab) is
  carried atomically from the click through to dispatch — no re-resolving the
  active layer at pick time (dropdowns.rs:377 and dispatch/params.rs:781-792 change
  shape; the modal crutch disappears).
- **D3 — One atlas, one bridge.** All visible cells render into one atlas texture
  (pattern: `content_pipeline.rs:2974-2999`), published over one triple-buffered
  Rgba16Float IOSurface `SharedTextureBridge`; UI samples per-cell UVs (pattern:
  `editor_bridge.rs:1580-1623`). *Rejected: one bridge per cell* — N triple-buffer
  surface sets, N `register_external_texture` calls, N-slot lifetime discipline per
  cell; the atlas machinery is production-proven today.
- **D4 — The audition pool lives on the content thread, owned by `ContentPipeline`;
  it never touches `Project` or `EditingService`.** Cells are standalone
  `PresetRuntime`/`Executor` builds (`PresetRuntime::from_def_with_device`,
  preset_thumbnail.rs:146), keyed `AHashMap<PresetTypeId, AuditionCell>`, each with
  its own `StateStore`. *Rejected: temporary undoable insert into the real chain* —
  pollutes the undo stack, churns snapshots, and makes a cancel path; audition is
  read-only by construction.
- **D5 — Commit is the existing command path, unchanged.** Click →
  `AddEffectCommand` / `ChangeGeneratorTypeCommand` as today; undo/redo semantics
  untouched. The browser never reopens on undo (status quo, kept deliberately —
  reopen-on-undo was considered and rejected: the perform gesture is "undo and pick
  again", not "watch the browser reappear").
- **D6 — Round-robin scheduling with budget back-off.** K cells per frame (initial
  K=2, visible-first); `frame_wall_ms` (`content_thread.rs:964`) is threaded into
  the pipeline and the whole audition block skips when the frame is over budget;
  `set_audition_visible(Vec<PresetTypeId>)` mirrors `set_node_atlas_visible`
  (content_pipeline.rs:1766) so a closed browser costs literally zero. *Rejected:
  fixed per-cell cadence* — a busy show frame shouldn't queue 27 cells of debt.
- **D7 — Stateful presets reset per browser open.** Watercolor, StylizedFeedback
  etc. start clean each open via the existing `clear_state` contract. Audition ≠
  committed-instance state; stated honestly: a feedback preset looks different in
  the audition grid than it will 30 seconds into a set. Accepted — choosing is a
  first-seconds question.
- **D8 — Crud removal**: NodeGraphTest and TrivialPassthrough move to test fixtures
  (references checked first — app_render.rs/app.rs, param_storage_v14.rs) ·
  Diagnostic is no longer user-facing: EdgeDetect and WireframeDepth recategorize to
  Stylize, BlobTracking gates `available=false` (parked per decision log; revive
  when the tracking work resumes) · LED-* generators are visible only on Dmx layers
  (data: a `layer_types: [...]` or equivalent on preset metadata; the picker filters
  by the invoking layer's type) · scene-asset companions (TimeScrub, Skin, MriVolume,
  SceneStarter) gate `available=false` with the Scene Setup panel as their host —
  they render nothing without their asset.
- **D9 — Recuration per audit table 1.3**: AutoGain→Color, DigitalDrift→Stylize,
  Dither→Stylize, EdgeDetect→Stylize, FilmGrain→Filmic, Glitch→Stylize,
  Infrared→Filmic, SoftFocus→Filmic, VoronoiPrism→Spatial; the rest hold. SceneStrobe
  generator renames to "Scene Strobe". Four-bucket taxonomy: Spatial / Color /
  Stylize / Filmic — Diagnostic leaves the filter row entirely.
- **D10 — One item model.** `PickerItem.badge` merges into `source`; a badge renders
  only for exceptional states (legacy stem-override, missing-from-library). Factory
  cells never carry a badge.
- **D11 — Generator browser gets the same UI as effects.** Category chips wired from
  `presetMetadata.category` (the field exists, it's dropped at dropdowns.rs:403);
  same cells, same nav, same audition.
- **D12 — The popup stays a popup** (D6 of the archived design, Peter: "I like the
  popup") but sizes to content and screen: cell grid fills available width up to a
  screen cap, cells denser than today (target ~6-8 columns at 1080p), caption strip
  actually backs the label with one text row (label left, badge right), real font
  metrics for chips, empty state ("No presets match"), search auto-focus at open.
- **D13 — Preset search gains aliases.** `search_text` on preset picker items,
  seeded from the registry's alias machinery (pattern: app_render.rs:1987-1994);
  "outline" finds Edge Detect.
- **D14 — Grid-shaped keyboard nav.** Arrow keys move in grid geometry, cursor
  calls `scroll_to_reveal` (scroll_container.rs:279), Home/End/PageUp/PageDown.
- **D15 — Strip engineering essays from preset `description` fields** (they are
  decomposition docs; move to docs/ or git). Metadata parse gets lighter.

## 3. Design body

### 3.1 Audition pool (content thread)

New module `crates/manifold-renderer/src/audition/mod.rs` (renderer crate: owns GPU
runtime; app crate wires transport). Owned by `ContentPipeline`, constructed with the
device handle it already holds.

```rust
pub struct AuditionPool {
    cells: AHashMap<PresetTypeId, AuditionCell>,
    atlas: AtlasLayout,            // cell rect packing, mirrors node-atlas layout math
    visible: Vec<PresetTypeId>,    // set via set_audition_visible, drained by scheduler
    round_robin: RoundRobinCursor,
}
struct AuditionCell {
    runtime: PresetRuntime,        // standalone build; effect cells bind the tap texture per frame
    target: RenderTarget,          // atlas cell rect at ATLAS_CELL_SIZE
    kind: PresetKind,
    state: StateStore,             // per-cell; reset on pool rebuild
}
```

API surface (called from `ContentPipeline` on the content thread only):

```rust
impl AuditionPool {
    pub fn set_visible(&mut self, ids: Vec<(PresetTypeId, PresetKind)>); // builds new cells, evicts the rest
    pub fn render_tick(&mut self, gpu: &GpuDevice, tap: AuditionTap, ctx: &PresetContext, budget_ok: bool);
    pub fn atlas_texture(&self) -> &GpuTexture;   // consumed by the shared-texture copy on the same encoder
}
pub enum AuditionTap<'a> { Master(&'a GpuTexture), Layer { layer_id: LayerId }, Clip { clip_id: ClipId } }
```

Effect cells pre-bind the tap texture per frame (pattern:
`MetalBackend::pre_bind_texture_2d`, preset_thumbnail.rs:339-345). Generator cells
render standalone. The pool is rebuilt when the browser opens (cells built while the
transport is stopped when possible; cold touches counted, not hidden — gate P2
reports them).

**Plausible-wrong architecture, forbidden by name:** you will want to insert a
temporary instance into the live chain and delete it on close — no (D4). You will
want one `Arc<Mutex<AuditionPool>>` shared to the UI thread — no: content thread
owns it, the UI sees pixels only, through the bridge. You will want to reuse the
static-thumbnail decode path for cells — no: that path is CPU PNG decode for
file-backed images; audition cells are GPU textures from frame to frame.

### 3.2 Transport (app crate)

One `SharedTextureBridge` instance + Rgba16Float IOSurface set for the audition
atlas (new instance of `shared_texture.rs` machinery, pattern at
`content_pipeline.rs:2974-2999` / `editor_bridge.rs:1580-1623`). UI registers the
IOSurface once via `register_external_texture` (ui_renderer.rs:744) and samples
per-cell UVs exactly like `graph_canvas::set_node_preview_src`. Publish only on
frames the atlas changed (`atlas_filled_this_frame` precedent, content_pipeline.rs:2034).

### 3.3 Browser popup (UI)

`BrowserPopupPanel` keeps its session model (the shipped P1–P2 overlay-session
design stays). Cell rendering changes: image from the audition atlas handle +
per-cell UV instead of `texture_handle_for_key(path)`; label row inside the caption
strip (left) with badge (right, exceptional states only); no space-padding prefix —
real x-inset. Layout recomputes columns from popup width (popup width = clamp to
screen, content-sized). Category chips from the item set's actual categories
(generators included). Search auto-focus at open; `search_text` on preset items.
Keyboard nav in grid geometry with scroll reveal; empty state row.

Invocation context (D2) rides the open request: `AddEffectClicked(tab)` becomes
`AddEffectClicked { tab, target: EffectTarget }` where `EffectTarget` names layer id
or master explicitly; dispatch stops re-resolving. Seam brief in P1: old → new
shapes written out, call-site inventory, compiler-driven migration.

### 3.4 Preset metadata changes (core)

- `presetMetadata` gains an optional `layer_types: Option<Vec<LayerType>>` — `None`
  = all layers; `Some([Dmx])` = DMX layers only (the 11 LED presets). Registry
  parsing + validation; the picker filters by the invoking layer's type.
- Recuration + renames are JSON edits (D8/D9); `available: false` becomes the
  standard gate (registry already honors it — NodeGraphTest proved the mechanism).

## 4. Invariants & enforcement

- **Audition never mutates the document.** Enforcement: negative gate —
  `rg 'AuditionPool|audition' crates/manifold-editing crates/manifold-core` returns
  zero hits (the pool lives in renderer/app only); plus the P2 test that a full
  audition session leaves `Project` byte-identical.
- **A closed browser costs zero per-frame work.** Enforcement: headless counter
  test — with the browser closed, `render_tick` is not called (visible list empty ⇒
  early return), asserted via a render-section trace or instrumentation counter.
- **No new shared state.** Enforcement: negative `rg 'Arc<Mutex|Arc<RwLock'`
  restricted to the new `audition/` module returns zero hits.
- **Context-atomic add: the layer an effect lands on is the layer whose button was
  clicked.** Enforcement: unit test on the dispatch path constructing a non-active
  layer, invoking, asserting the command targets it; L3 flow if the harness can
  reach the inspector.
- **LED presets unreachable from video-layer pickers.** Enforcement: picker-core
  filter test over the item list for both layer types.

## 5. Phasing

One phase = one session; each ends committable with gates run by the orchestrator.

- **P1 — Crud + context-atomic adds.** Deliverables: recurated JSON set (D8/D9),
  NodeGraphTest + TrivialPassthrough to fixtures, `layer_types` metadata field +
  filtering, badge/source merge (D10), `EffectTarget` seam (D2), generator category
  chips wired (D11), description strip (D15), SceneStrobe rename. Gates: preset
  validation suite green, `graph-tool validate` over every touched JSON, picker
  item-construction tests (LED gating, category presence), negative rg for removed
  preset files. Demo: headless PNG of both browsers post-recuration — L2.
- **P2 — Audition engine + vertical slice.** Deliverables: `audition/` module,
  pool + round-robin + budget skip, `set_audition_visible` plumbing, tap selection
  (master + layer), atlas bridge instance, minimal browser cell wiring (live
  texture in place of static thumb). Gates: pool test (visible-set drives renders,
  closed = zero), budget-skip test, `MANIFOLD_RENDER_TRACE=1` run with browser open
  (no frame >20ms), cold-touch report at open, value test: a cell's rendered pixels
  differ between two presets applied to the same synthetic tap (computed, not
  eyeballed). Demo: headless render of the browser with live cells — L2 minimum,
  L3 flow if reachable.
- **P3 — Browser polish + nav.** Deliverables: dense content-sized layout, caption
  strip/label row alignment, font-metric chips, empty state, search auto-focus +
  aliases (D13), grid keyboard nav + scroll reveal (D14), dead-code cleanup
  (F14–F17), stale accent table, wheel hit-testing. Gates: ui snapshot tests,
  negative rg for deleted symbols, L3 flow covering open → search → cursor → add →
  undo.

## 6. Decided — do not reopen

1. Live audition on the real show frame, all visible cells, round-robin. No static
   thumbnails in browser cells.
2. Audition input = invocation context (master / layer / future clip). No
   "audition on a neutral test card" option.
3. One atlas + one IOSurface bridge. No per-cell bridges.
4. Audition is read-only; commit = existing command path; no undo involvement.
5. The popup stays a popup (Peter: "I like the popup").
6. Diagnostic category leaves the user-facing browser; LED presets are DMX-only;
   scene-asset companions gate behind their host panel.
7. Four effect buckets: Spatial / Color / Stylize / Filmic.
8. Browser closed ⇒ zero audition cost. Non-negotiable budget rule.

## 7. Deferred

- Clip-slot invocation of the browser (the Clip arm exists dead today) — revive
  when clip-level effect slots are designed.
- Hover-audition on the program output as a separate perform gesture — the grid
  subsumes the choosing use; a "try on my mix" mode is a later toggle.
- Audition of *param variations* (turning knobs inside the audition grid) — real
  value, real scope; trigger: after P3 lands and Peter uses it live.
- Regenerating static thumbnails for file-picker consumers (project file dialogs)
  — the one-shot bin still works; only do it if a non-browser consumer appears.
- Alias authoring UI (letting Peter edit preset aliases) — aliases ship seeded from
  registry data; UI waits for demand.
