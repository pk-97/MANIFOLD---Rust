# Preset Library — one mental model for presets, a library you can see, and explicit save/revert/push

**Status:** APPROVED design, not built · 2026-07-04 · Fable
**Prerequisites:** none for P1–P4; P5 (browser) has a hard edge on OVERLAY_SESSIONS_AND_PICKER P2; P6 (thumbnails) is verify-at-impl gated.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

Peter, 2026-07-04: **"I want to rethink these categories from the ground up. The
current model we use isn't easy to follow or intuite"** — the categories being
stock / user dir / project forks / per-instance edits. The governing insight from
the audit: the *storage* is fine and mostly stays; what's broken is that four
storage tiers are presented as four concepts, edits diverge through two different
invisible mechanisms, and the only library door is a native file dialog. The fix is
**one user-visible rule — an instance follows its library entry until you edit it;
then it's yours, visibly, until you revert or save it back** — plus a browser that
shows the library as a place.

This supersedes attempt #8's fork ergonomics (auto-fork-on-shared-edit, `#N`
variant ids — `project_preset_unification` memory, 2026-06-06). The audit found the
auto-fork gate was designed but **never wired** (`count_preset_uses` has zero
production callers), so what ships today is already tracking-until-edit for
definition edits, plus an explicit fork action. This design keeps the live
mechanism, names it, surfaces it, and deletes the unwired machinery rather than
finishing it.

A note on the chat trail: the discussion phrase was "copy on drop." The committed
model is **copy on first edit** (tracking until then). Same experienced rule —
edits never silently affect other instances; propagation is explicit — but it
preserves the live preset-JSON hot-reload authoring loop (edit `Bloom.json`, every
untouched Bloom updates — the DECOMPOSING_GENERATORS workflow), keeps project files
small, and matches what the runtime already does.

Companion docs: `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` (the browser rides its
PickerCore) · `CLIP_THUMBNAILS_DESIGN.md` (thumbnail transport precedent) ·
`docs/FREEZE_COMPILER_MAP.md` (read before touching resolution — freeze keys off
effective defs).

## 1. Audit — what exists (verified 2026-07-04)

| Piece | Where | State |
|---|---|---|
| Catalog tiers | `crates/manifold-renderer/src/preset_loader.rs:1-37` | Stock (bundle/dev assets) + user dir (`~/Library/Application Support/MANIFOLD/presets/{effects,generators}`) + project overlay (`set_project_presets`). User overrides stock by filename stem. Hot-reload watcher (1s mtime poll), arc-swap RCU, fail-loud on empty stock. |
| Instance | `crates/manifold-core/src/effects.rs:607` (`PresetInstance`), `graph: Option<EffectGraphDef>` at :667 | `effect_type: PresetTypeId` names the library entry; `graph: None` = tracking, `Some` = diverged private copy. |
| Divergence mechanism A (live) | `crates/manifold-editing/src/commands/graph.rs:307` — `inst.graph.get_or_insert_with(|| catalog_default.clone())` | Every graph-editor definition edit bakes the catalog def into the instance on first touch. Silent — no UI shows tracked vs diverged. |
| Divergence mechanism B (explicit) | `ForkPresetCommand` (`crates/manifold-editing/src/commands/preset.rs:26`), invoked from `MakePresetUnique` (`ui_bridge/inspector.rs:2348`) and `ImportPreset` (:2397) | Mints `Base#N` id (`project.rs:803`), registers an `EmbeddedPreset` (`project.rs:25`), retargets the instance. |
| Divergence mechanism C (designed, dead) | `count_preset_uses` (`project.rs:773`), `EditPresetParamCommand` (`preset.rs:147`) | Auto-fork-on-shared-edit gating: **zero production callers**. Dead code from attempt #8's unfinished Phase 4 UI. |
| Runtime resolution | `crates/manifold-renderer/src/preset_runtime.rs:672` — `fx.graph.as_ref().unwrap_or(view.canonical_def)` | Instance graph wins; else catalog view (project overlay → user → stock). |
| `#N` display hack | `card_preset_name` (`ui_bridge/state_sync.rs:1505`) splits ids on `'#'` to render "(variant)" | Id-format-as-UI — the illegibility made flesh. |
| Save out | `ExportPreset` → native save dialog (`ui_bridge/inspector.rs:2371`); `manifold-io/src/preset_file.rs` (`export_preset`/`import_preset`) | The ONLY library door. No save-to-user-dir, no rename/delete/duplicate anywhere in-app. |
| Browser | `browser_popup.rs`; item lists built at `ui_root.rs:1420-1490` from `preset_type_registry::available_of_kind` + embedded presets (category "Project") | Embedded presets already listed. No Factory/User distinction, no management, no thumbnails. |
| Save format | v1.10.0 current (`manifold-io/src/migrate.rs:68`); `Project.embedded_presets` (`project.rs:76`) serialized, skipped when empty | Projects referencing user/stock ids are NOT self-contained — a deleted user file strands old projects. ⚠ VERIFY-AT-IMPL: what a missing preset id does at load today — load a project referencing a nonexistent id, read the failure path. |
| Thumbnail precedents | node preview atlas (editor), `CLIP_THUMBNAILS_DESIGN.md` transport clone; headless PNG harness (`reference_ui_headless_png_verification` memory) | No static-image cell in the bitmap UI browser. ⚠ VERIFY-AT-IMPL: whether `manifold-ui` can draw a loaded PNG in a popup cell — check how node previews blit, `rg "preview" crates/manifold-ui/src/graph_canvas/`. |

**Extend, don't redesign.** Loader tiers, arc-swap catalogs, hot-reload,
`EmbeddedPreset`, `with_preset_graph_mut`, and mechanism A all stay.

## 2. Decisions

- **D1 — One divergence rule: tracking until first definition edit.**
  `graph: None` = the instance follows its library entry (hot-reload included);
  any definition edit (topology, ranges, exposure, metadata) bakes and edits the
  private copy. Mechanism A, kept and promoted to *the* rule. Param **values**,
  drivers, envelopes, mappings are instance state and never diverge the definition.
  *Rejected:* copy-on-drop (kills the JSON hot-reload authoring loop, bloats
  saves); auto-fork-on-shared-edit (attempt #8 — the invisibility Peter is naming;
  also literally never wired).
- **D2 — Delete the dead fork machinery, keep the explicit action.**
  `count_preset_uses` and `EditPresetParamCommand` are deleted (compiler-driven).
  `MakePresetUnique` survives as the explicit "detach into a project preset"
  action; `ForkPresetCommand` stops minting `#N` and mints human names ("Bloom 2")
  written to `display_name` AND id; `card_preset_name`'s `'#'` parsing is deleted
  (display names carry the information). Legacy `#N` ids in existing projects keep
  resolving (they live in `embedded_presets`; resolution is untouched).
- **D3 — Divergence is visible and reversible.** The card and graph editor show a
  "modified" badge when `graph.is_some()` (trivially derivable, no hashing); the
  actions are **Revert to Library** (`graph = None`, undoable) and **Push to
  Library** (write the instance def to the library entry's file — other tracking
  instances update via the existing hot-reload; the action text says so).
  *Rejected:* content-hash comparison against the library def to detect
  "modified-but-identical" — cost without a decision it changes; `graph.is_some()`
  is the honest bit.
- **D4 — Explicit library doors: Save to Library, Save to Project.** Save to
  Library writes `<user-root>/{effects,generators}/<Name>.json` (id minted from
  the name, disambiguated "Name 2" — never colliding with an existing stock or
  user id, so no NEW override-by-stem is ever created). Save to Project upserts an
  `EmbeddedPreset` with `origin: Saved`. Export/Import file dialogs stay, as
  sharing, not as the library door. Legacy stem-overrides already on disk keep
  working (resolution unchanged) but the browser badges them "overrides Factory".
- **D5 — Projects become self-contained via snapshot-on-save.** At save,
  every library id referenced by a tracking instance gets its current def
  upserted into `embedded_presets` with `origin: Snapshot`; stale snapshots
  (unreferenced, `origin: Snapshot`) are pruned. Resolution order changes for
  snapshot entries only: **disk wins, snapshot is the fallback** — so an improved
  user preset isn't shadowed by an old project's cache, but a deleted file no
  longer strands the project. `origin: Saved` entries keep today's on-top
  behavior (they're deliberate). Serde: `origin` defaults to `Saved` on old files.
  *Rejected:* per-instance def baking at save (file bloat, kills tracking);
  content-hash interning tables (complexity — ZIP compression plus one-def-per-id
  snapshots already dedupe).
- **D6 — Browser: source is a filter dimension, not a hierarchy.** One flat
  searchable space; a source row (All · Factory · My Library · This Project) above
  the existing category chips; origin badges on cells. Management (rename,
  duplicate, delete, reveal-in-Finder for user entries) via right-click in the
  browser — the browser IS the manager, no separate screen. Peter: **"I like the
  popup"** — it stays the insert-flow popup, no sidebar.
- **D7 — Thumbnails render at save time, not browse time.** Save to Library
  renders a 256px PNG (headless harness; generators render bare, effects over the
  parity harness's standard input) stored as `<Name>.png` beside the JSON. Factory
  thumbnails come from a one-shot dev bin committed to assets. Browse time never
  renders. Own phase, verify-at-impl gated (§1 last row).
- **D8 — `effect_type` survives as the based-on id.** It's the serialization
  anchor (type ids are forever), the Ableton addressing key
  (`find_preset_instance_mut`), and the provenance link. It stops implying "the
  graph comes from the catalog" — that's `graph: None`'s job.

Consequences, stated honestly: **a tracking instance changes when its library file
changes.** That's the deliberate cost of keeping the authoring loop live — a stock
update or a Push to Library reaches every untracked instance in every open
project. The mitigations are D3's badge (you can see which instances are yours)
and D5 (a saved show pins its defs as snapshots; a gig machine that never edits
JSON never sees a surprise). The stage-safety property Peter asked for — "the same
preset behaves the same at the gig" — comes from D5's self-containment, not from
freezing instances.

## 3. The model, in instrument terms

Three places a look can live, one rule connecting them:

- **Factory** — read-only, ships with the app.
- **My Library** — your folder, survives projects. Save to Library puts a look
  here; rename/delete/duplicate in the browser; edits to these files reach
  tracking instances live.
- **This Project** — travels in the `.manifold`: looks you saved to the project
  (`Saved`) plus the automatic snapshots that make the file self-contained
  (`Snapshot`, invisible in the browser by default — they're plumbing, listed
  only when their source file is gone, badged "missing from library").

An instance dropped from any of them **tracks** its entry until you edit its
definition; then the card says modified and offers Revert / Push / Save as new.
On stage that means: trust the badge — an unbadged card is exactly the library
sound; a badged card is yours and survives any library change.

## 4. Committed shapes

```rust
// manifold-core/src/project.rs — EmbeddedPreset grows one field
pub struct EmbeddedPreset {
    pub kind: PresetKind,
    pub def: EffectGraphDef,
    /// Saved = user pressed "Save to Project" / "Make Unique" / import.
    /// Snapshot = auto-captured at save for self-containment (D5); pruned +
    /// refreshed every save; resolution: disk wins over Snapshot.
    #[serde(default)]                       // legacy files → Saved
    pub origin: EmbeddedOrigin,
}
#[derive(Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
pub enum EmbeddedOrigin { #[default] Saved, Snapshot }

// manifold-editing/src/commands/preset.rs — replaces the deleted pair
pub struct RevertToLibraryCommand { target: GraphTarget, old_graph: Option<EffectGraphDef> }
// execute: old_graph = inst.graph.take(); undo: restore. Fails loud (no-op +
// log) if the library id no longer resolves — reverting to nothing is worse
// than staying diverged.

// manifold-app (new module, e.g. src/user_library.rs) — the file-ops service;
// UI-thread, no shared state; all ops go through std::fs + the existing watcher
// picks up changes. NOT a crate-boundary crossing: file IO for the user dir
// already lives app-side (preset_loader resolves the same root).
pub struct UserLibrary { root: PathBuf }
impl UserLibrary {
    pub fn save(&self, kind: PresetKind, name: &str, def: &EffectGraphDef) -> Result<PresetTypeId, LibError>; // mints non-colliding id from name
    pub fn rename(&self, kind: PresetKind, id: &PresetTypeId, new_name: &str) -> Result<(), LibError>;        // display_name edit; id + filename stay
    pub fn duplicate(&self, kind: PresetKind, id: &PresetTypeId) -> Result<PresetTypeId, LibError>;
    pub fn delete(&self, kind: PresetKind, id: &PresetTypeId) -> Result<(), LibError>;                        // user entries only; never factory
    pub fn reveal(&self, kind: PresetKind, id: &PresetTypeId);                                                // open in Finder
}
```

Push to Library = `UserLibrary::save` targeting the existing entry's file (user
entries only; pushing over a factory id offers Save-to-Library-as-new instead —
factory files are read-only in the bundle).

The plausible-wrong architecture, forbidden by name: **you will want to make
instances always own their graph ("simpler!").** No — that kills the hot-reload
authoring loop and re-creates the propagation problem as file bloat. **You will
also want a use-count check somewhere.** No — counting users is the machinery
this design deletes; no edit path may consult how many instances share an id.

## 5. Phasing

Test-scope note: P1 and P2 touch core resolution/serialization — each ends with
the full workspace sweep. P3–P6 are UI/app-scoped — focused tests + manual pass,
no sweep.

### P1 — One rule: delete the dead fork machinery, humanize explicit forks

- **Entry state:** re-run the inventory: `rg -n "count_preset_uses|EditPresetParamCommand" crates/` → definition + tests only (if production callers appeared since 2026-07-04, STOP and list them); `rg -n "split_once\('#'\)" crates/manifold-app/src/ui_bridge/state_sync.rs` → 1 hit.
- **Read-back:** this doc §2 D1/D2, §4 forbidden-by-name; `preset.rs` whole file; `state_sync.rs` `card_preset_name`.
- **Deliverables:** delete `count_preset_uses` + `EditPresetParamCommand` (+ their tests); `ForkPresetCommand` mints display-based ids ("Bloom 2" — reuse `mint_embedded_preset_id` with a `" {n}"` probe instead of `#{n}`, keep the `'#'`-tolerant loader behavior for legacy ids); embedded `display_name` set to the minted name; delete `card_preset_name`'s `'#'` split (embedded presets render their `display_name`); load-time cosmetic pass: legacy `#N` embedded presets get `display_name = "Base (variant)"` if unset.
- **Gate (positive):** `cargo clippy --workspace -- -D warnings`; full `cargo test --workspace`; Liveschool fixture round-trip green; manual: Make Unique on a shared effect → card shows "Bloom 2", other instances unaffected.
- **Gate (negative):** `rg -n "count_preset_uses|EditPresetParamCommand" crates/` → 0 hits; `rg -n "split_once\('#'\)" crates/manifold-app/src` → 0 hits.
- **Forbidden moves:** wiring the shared-edit gate "since we're here" · keeping EditPresetParamCommand "for later" · touching resolution order (that's P2).

### P2 — Self-contained saves (snapshot-on-save)

- **Entry state:** P1 merged. Run the §1 VERIFY-AT-IMPL for missing-id load behavior; write the observed failure into the phase notes (it becomes this phase's before/after proof).
- **Read-back:** D5; `project_io` load path where `set_project_presets` is installed; `preset_loader.rs` `build_catalog` merge order.
- **Deliverables:** `EmbeddedOrigin` field (serde default `Saved`); save path collects referenced ids from tracking instances (effects, clip effects, master, generators) and upserts `Snapshot` defs + prunes stale ones; catalog merge treats `Snapshot` entries as below disk tiers, `Saved` entries on top (today's order); io round-trip tests for both origins.
- **Gate (positive):** full workspace sweep + Liveschool golden; new io test: save project referencing a user preset → delete the user file → reload → instance renders from snapshot with a loud log line; report Liveschool `.manifold` file size before/after (expect small growth; escalate if >5MB delta).
- **Gate (negative):** `rg -n "origin" crates/manifold-io/src` shows serde default (legacy files load as `Saved`); no resolution change for `Saved` ids (existing test suite is the proof).
- **Forbidden moves:** interning/hashing schemes · baking graphs into instances · changing `Saved` resolution order.

### P3 — Library doors + the file-ops service

- **Entry state:** P1 merged (P2 independent). ⚠ VERIFY-AT-IMPL: new-file freshness — drop a JSON into the user dir while running; confirm the Add browser lists it without restart (the 2026-06 memory claims a stale `OnceLock` picker path; the registry is arc-swap now — observe, don't recall). If stale, fixing that staleness joins this phase's deliverables.
- **Read-back:** D4; §4 `UserLibrary`; `preset_file.rs`; the ExportPreset dispatch arm (`inspector.rs:2371`).
- **Deliverables:** `UserLibrary` service per §4; `SaveToLibrary` + `SaveToProject` panel actions on the card menu and graph editor (name prompt via existing text-input session, owned per OVERLAY_SESSIONS D2); Save to Project upserts `origin: Saved`.
- **Gate (positive):** focused `-p manifold-app` + `-p manifold-io` tests; manual: save a tweaked Bloom to Library → appears in browser (both kinds tested); save to Project → travels through save/reload; clippy.
- **Gate (negative):** `rg -n "rfd::FileDialog" crates/manifold-app/src/ui_bridge/inspector.rs` → hits only in Export/Import arms (library saves never open a dialog).
- **Forbidden moves:** writing library files from `manifold-ui` or `manifold-core` · silent overwrite on name collision (disambiguate) · deleting factory files.

### P4 — Divergence made visible (badge · Revert · Push)

- **Entry state:** P3 merged (Push needs `UserLibrary`).
- **Read-back:** D3; card config build in `state_sync.rs`; `RevertToLibraryCommand` shape in §4.
- **Deliverables:** modified badge on card + editor header when `graph.is_some()`; `RevertToLibraryCommand` (undoable, fails loud per §4); Push to Library action (user-library entries; factory offers save-as-new); context-menu wording states the blast radius ("updates N tracking instances" is NOT computed — it says "updates instances tracking this preset").
- **Gate (positive):** focused app/ui tests + manual: edit a graph → badge appears; Revert → badge gone, pixels match library (visual check); Push → a second tracking instance updates live.
- **Forbidden moves:** any use-count computation (§4 forbidden-by-name) · hash-based modified detection.

### P5 — Browser: sources, badges, management (hard edge: OVERLAY_SESSIONS P2)

- **Entry state:** OVERLAY_SESSIONS P2 merged (`PickerItem` exists — re-verify: `rg -n "struct PickerItem" crates/manifold-ui/src`).
- **Read-back:** D6; §3; `ui_root.rs:1420-1490` open sites; PickerCore API.
- **Deliverables:** source filter row (All · Factory · My Library · This Project) as picker state above category chips; `PickerItem.badge` populated from origin; right-click management menu on user/project cells (rename → text session, duplicate, delete with confirm, reveal); `Snapshot` entries listed only when their id is missing from disk, badged "missing from library".
- **Gate (positive):** manual matrix: each source filter × search × category; rename/duplicate/delete round-trip visible in the browser without restart; focused ui tests; clippy.
- **Forbidden moves:** a separate library-manager window · folder trees · touching dropdown/ableton_picker.

### P6 — Thumbnails (conformance level — verify-at-impl heavy)

- **Entry state:** P3 + P5 merged. Pre-flight (§1 last row): confirm or refute a static-image draw path in the popup UI; if absent, **escalate with the finding** — the options (extend the node-preview blit vs a small image-cell node type) are an architecture choice Peter signs off, not an executor call.
- **Deliverables (shape, pinned after pre-flight):** save-time 256px PNG render via the headless harness (generators bare; effects over the parity standard input); `<Name>.png` beside the JSON; factory-thumbnail one-shot bin; browser cells render the image with text fallback.
- **Gate (positive):** browser shows images for entries that have them, clean fallback for those that don't; save-to-library produces a PNG that Read-the-file confirms is the look; clippy + focused tests.
- **Forbidden moves:** browse-time rendering · per-frame texture uploads for static cells.

## 6. Decided — do not reopen

1. Tracking-until-first-definition-edit is THE divergence rule; no auto-fork, no
   use counts, ever.
2. `count_preset_uses` + `EditPresetParamCommand`: deleted, not finished.
3. Copy-on-drop rejected (hot-reload authoring loop + file size).
4. Self-containment = snapshot-on-save (`origin: Snapshot`, disk-wins resolution);
   not per-instance baking, not interning.
5. Library doors are explicit actions; file dialogs are for sharing only.
6. Modified = `graph.is_some()`. No hashing.
7. Browser stays the insert popup (Peter: "I like the popup"); source is a filter;
   management is right-click in place.
8. `effect_type` stays (serialization, Ableton addressing, provenance).
9. New ids never collide with existing stock/user ids; legacy stem-overrides keep
   resolving but are badged, and no path creates new ones.
10. Thumbnails at save time only.

## 7. Deferred

- **Apply-to-siblings** ("update every *modified* instance based on Bloom") —
  Revert + Push covers the tracked case; trigger: Peter asks for it after living
  with P4.
- **Param-variation presets** (same graph, different knob positions, à la Ableton
  .adv vs device) — today's answer is Save to Library as a new entry; trigger:
  library sprawl from near-duplicate graphs.
- **Persistent browser sidebar / browse-for-inspiration surface** — trigger:
  browsing becomes a real session activity (Peter's call).
- **Hover-preview on program output** (Resolume-style preview bus) — trigger:
  post-thumbnails, if stills prove insufficient on stage.
- **TextInputState session collapse** — tracked in OVERLAY_SESSIONS §7.
