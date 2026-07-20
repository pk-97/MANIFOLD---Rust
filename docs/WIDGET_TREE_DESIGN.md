# Widget Tree — the queryable param-surface layer at the UI↔engine boundary

**Status: IN PROGRESS — APPROVED by Peter 2026-07-20 ("You must orchestrate this session in full until all work is finished… Sonnet 5 low-effort agents"); P1a + P3 LANDED 2026-07-20 (`f623a474` lineage, landing report `docs/landings/2026-07-20-widget-tree-p1a-p3.md`); P1b LANDED 2026-07-20 (types `f53ff1f0` Fable + swap `ecb109fb` lane — `ParamCardConfig`/`ParamInfo` deleted, one projection `state_sync::param_surface`); P2 LANDED 2026-07-21 (RowIndex routing, keyed identity incl. card roots via opt-in `View::identity`, `no_bespoke_row_infra`; landing report `docs/landings/2026-07-21-widget-tree-p2.md`); P4 LANDED 2026-07-21 (twins found already-collapsed since `79905d63` — audit row corrected in §1; `row_dispatch` master+layer Harness family shipped, closing P2's owed test gaps); P5 CLOSED 2026-07-21 — design COMPLETE (landing report `docs/landings/2026-07-21-widget-tree-p5-close.md`): scene sections ride the identical host by construction (scene convergence + BUG-295 live refresh landed `d3ad7502`), flow accounting 14/15 scene-setup flows green (modifier-stack = VD-035/BUG-293), zero `ParamCardConfig`/`match_param_row_click` hits in crates/, card-drag L3 artifact blocked on BUG-296 (VD-034 burn-down updated), editor surface at L2 per VD-030; Fable orchestrates and lands, Sonnet lanes execute mechanical phases, one commit per lane · 2026-07-20 · Fable · freshness pass 2026-07-20 (re-entry session): independent re-audit converged on the same architecture; anchors re-verified post-W2-B (P3 addendum, INV-3 restated); scrub-engine alternative recorded in Deferred · ADVERSARIALLY REVIEWED by a Fable fork 2026-07-20 (2 blockers + 4 gaps, all folded in: D4 keying spec, §5b agent contract, P1a/P1b split, P2 `register_intents`, pinned hasher)**
**Prerequisites:** W2-B (BUG-265 root fix — tree-bounds hit-testing) — **LANDED on main 2026-07-20 (`f2ac71d9`; the orchestrator fixup `94632d65` also deleted `card_y` outright), so P3's prerequisite is satisfied and its scope shrank (see the P3 addendum).** SCENE_PANEL_EXPOSURE_CONVERGENCE P2 (deletes the scene funnels) before P5 — convergence is executing on `lane/scene-panel-exposure-convergence` (P1 + P2 slice 2a committed on the lane as of 2026-07-20). UI_WIDGET_UNIFICATION (SHIPPED) and INPUT_IDENTITY_UNIFICATION (SHIPPED, archived) are the groundwork this rides.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

**The governing insight: "what a param row is" already exists exactly once in the engine — the param manifest (`inst.params`, one id-keyed walk, authority resolved at instantiation) — but between that manifest and the pixels sit four hand-written transcriptions that each re-state it: a parallel-vec DTO (~15 position-indexed arrays), an imperative 1,300-line tree build, an id-hoard click gauntlet duplicated per card kind, and per-family dispatch in the bridge. Every card fact lives in 5+ places; a forgotten mirror compiles and silently defaults.** That is the disease that produced the scene panel's imitation layer (BUG-237/249/250/260), the dead-click class, and "fixed for Master, forgot Layer." This design replaces the four transcriptions with one queryable model: a projection the engine writes once and the renderer, the gesture router, the tests, and the automation selector all read.

The stage translation: this layer is why the next card affordance ships by editing one function instead of five files, why a dead click or a lying display becomes a test that fails in seconds instead of a bug Peter finds mid-set, and why a lane briefed with card work physically cannot rebuild a parallel system — the queryable model is the path of least resistance. Peter's frame for the scene panel generalizes to the whole boundary: *"all one unified system"* (2026-07-19). The testing doctrine this serves, verbatim from the plan: *"Pixels are for looking, not asserting"* (2026-07-20). And Peter's goal directive for the layer itself, verbatim (2026-07-20, at adversarial review): *"The goal is a simple, safe, fast, efficient, and easy to implement UI and UX infrastructure layer for agents to work with. Agents must never create their own infra for basic things like rows, sliders, drawers, etc ever again during implementation."* The second sentence is a standing rule, not an aspiration — §5b turns it into machine enforcement.

**Binding constraints** (DESIGN_AUTHORING §1, checked): *Hot path* — card VALUE sync runs per-frame (`app_render.rs:4407`); the structural projection must stay on the data-change path, never per-frame. *Thread residency* — UI thread reads `Arc<Project>` snapshots; the projection is UI-side read-only; mutations stay `PanelAction` → `ui_bridge::dispatch` → `ContentCommand`. *Time model* — untouched (params are floats/enums; beats never appear here). *Persistence* — none; the model is a projection, never serialized. *Performance surface* — every card row IS a live control; MIDI/OSC/Ableton/automation state are row facts in the model, not later phases.

Companion docs: `SYSTEM_UPGRADE_2026_07_PLAN.md` (diagnosis + doctrine), `SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` (the autopsy; its end state is a consumer of this layer), `INSPECTOR_DRAG_TAB_FINDINGS.md` (BUG-265/266/267 — the geometry half), `UI_WIDGET_UNIFICATION_DESIGN.md` (surface-agnostic widget contracts, shipped), `docs/archive/INPUT_IDENTITY_UNIFICATION.md` (WidgetId — the stable identity this keys on), `UI_AUTOMATION_DESIGN.md` (the selector DOM that gains row addressability).

---

## 1. Audit — what exists (verified 2026-07-20)

| Piece | Where | State |
|---|---|---|
| Param manifest — the single authority | `PresetInstance.params`; consumed at `state_sync.rs:3104–3127` (`rows_from_manifest` :3043) | EXISTS. Authority chain resolved once at instantiation/load (PARAM_STORAGE_BOUNDARIES D4/D12). The model's only source. |
| Transcription #1: manifest → DTO | `state_sync.rs` `preset_to_config` :3092–3299, `build_card_modulation` :2547, `build_audio_card_state` :2632 | DIES. Hand-assembles the parallel-vec DTO + `id_to_index` positional map. |
| The parallel-vec DTO | `param_card.rs` `ParamCardConfig` :305–373 (~15 per-param `Vec`s), `ParamInfo` :117–179 | DIES. Replaced by id-keyed `ParamRow`s. |
| Transcription #2: DTO → tree | `param_card.rs` `build` :1932–3210 (imperative node minting + id hoarding), `sync_values` :3211 | REWIRED. Build survives as the renderer, re-pointed at rows; the `Vec<Option<NodeId>>` hoards die in P2. |
| Transcription #3: id-hoard click gauntlet | `param_card.rs` `handle_click` :3649, `handle_click_effect` :3673 / `handle_click_generator` (twin), `match_param_row_click` (10 parallel arrays), `handle_pointer_down` :4104, `handle_drag` :4353 | DIES (P2). Replaced by the WidgetId-keyed row index + one role→action function. |
| Transcription #4: per-family bridge dispatch | `ui_bridge/mod.rs` `dispatch` :158 (18 args); `ui_bridge/inspector.rs` `resolve_graph_target` :224, dual-edit helpers :62–223; scene funnels `resolve_scene_write` :256, `scene_bound_slot` :297, `resolve_mod_target` :401 | **CORRECTED at P4 execution (2026-07-21): the effect/generator twins were already collapsed over `GraphParamTarget` by `79905d63` (2026-06-08, "unify all UI dispatch arms") — this audit row overstated the remaining disease.** P4 verified one generic handler per action kind and shipped the `row_dispatch` master+layer Harness family instead. Scene funnels still die via convergence P2 (not this design). 18-arg `dispatch`: Deferred, §10. |
| Geometry snapshots (BUG-265's disease) | `card_y` — **DELETED 2026-07-20 by W2-B's landing (`94632d65`)**; `compute_height` (+ effect/generator twins) survives with ~20 call sites in `panels/inspector.rs`, all in `build()`'s layout cursor + the column-height helpers (`master_column_height` :997, `layer_column_height`) — build-time layout, no longer a post-build hit-test source (re-verified 2026-07-20, post-W2-B) | Post-build consumers DIED with W2-B. P3 shrinks to: prove no post-build consumer remains, mid-tween geometry test family, INV-3 as restated. |
| Stable widget identity | `WidgetId` `node.rs:295–350` (path-derived, rebuild-stable, splitmix64); keyed builders `tree.rs:234` (`add_node_keyed`), :433; reorder-stability proven `tree.rs:1747–1781`; input tracks by WidgetId | EXISTS (shipped). The row index keys on it. |
| Selector DOM + dump | `manifold-ui/src/automation.rs` `SelectorQuery` :117 (name/text/type/under_text/nth), `AutomationTarget::Widget(u64)` :64; `ui_snapshot/dump.rs` serializes durable widget ids | EXISTS. Rows become addressable by param-id-derived keys for free once P2 keys them. |
| Widget gesture contracts | UI_WIDGET_UNIFICATION SHIPPED P1–P8 (right-click reset, steppers, contract derivation) | EXISTS. Row widgets keep their contracts; this design feeds them identity, not behavior. |
| Intent-at-build + right-click contracts | `IntentRegistry<A>` (generic since overhaul Phase 6); `param_card.rs` `register_intents` :4668–4760 — NOT "partial": it is the widget-contract right-click layer (BitmapSlider `register_track_reset`/`register_label_mapping`, the BUG-070/BUG-105 fixes) and it READS the id-hoards P2 deletes (`slider_ids`, `audio_configs`, `envelope_config_ids`, `relight_slider_ids`, `row_catcher_ids`, `pid_at`) | EXISTS. P2 re-points it at rows explicitly (fork-review GAP-2); right-click stays on the widget-contract path (widget-unification D5), never migrated into `RowIndex`. |
| Per-frame value channel | `sync_card_values` `state_sync.rs:963` → `param_slots_to_ui` → `sync_values(&[UiParamSlot])`; called per-frame at `app_render.rs:4407` + main-window push_state | KEPT. Order defined by the manifest walk; positional stream stays, length-asserted against rows. |
| State-level test harness | `ui_bridge/inspector.rs` `undo_baseline` :5217, `mapping_undo_baseline` :6569, `bug_266_tab_pin` :6677 — `Harness` + `ContentSide` over a real `EditingService` | EXISTS. The click→command test family (§6) replicates this pattern, never invents harness. |
| Card storage unification | BUG-267 landed `717f8910` — one card vec keyed by scope in `panels/inspector.rs` | EXISTS. Build on it. |
| Scene panel | Convergence APPROVED, executing P1→P4 on `lane/scene-panel-exposure-convergence` (P2 makes scene rows real card rows) | IN FLIGHT. P5 of this design consumes its end state; nothing here touches scene code before that lands. |
| Runtime card state (drag, drawers, tweens) | `ParamCardState` :516, `SliderDragState`, drawer animators | KEPT AS-IS. Runtime widget state, not transcription — the chrome-declarative/widget-imperative split is the architecture (overhaul §13), not a gap. |

Classification: **exists** — the manifest, WidgetId, keyed builders, selector DOM, widget contracts, the test harness, the per-frame value channel. **One wire away** — cards already unified across kinds (`preset_to_config` is one path), actions already carry `(target, ParamId)`, `register_intents` partially rides the registry. **Genuinely new** — the `ParamRow`/`ParamSurface` types, the one projection function, the WidgetId-keyed row index, the one role→action function. Four small pieces; everything else is deletion and re-pointing. The design shrank in the audit, as it should.

Negative claims, checked: no existing generic row model in `manifold-ui` (`rg 'struct ParamRow' crates/` — zero hits); no other consumer of `ParamCardConfig` outside `param_card.rs`/`state_sync.rs`/editor config assembly (`rg 'ParamCardConfig' crates/ -l` — 4 files: the two above + the editor's card assembly + `panels/mod.rs` re-export).

---

## 2. Decisions

**D1 — The layer is a projected view-model (`ParamSurface`), built app-side by ONE function, for every manifest-backed surface.** Master effects, layer effects, generator cards, graph-editor cards (`CardContext::Author`), and — after convergence — scene sections are all projections of the same shape. Precedent: the Phase-5 `ui_translate` view-model pattern (`UiLayer`, `UiParamSlot`); this is its param-surface sibling. Rejected: *reading `Project` directly from `manifold-ui`* — the layering inversion is settled (ui depends only on foundation, overhaul Phase 5); un-inverting it for cards drags core types into the UI crate and kills standalone testability. Rejected: *moving the manifest types into foundation so no projection exists* — `PresetInstance` carries engine state (graphs, bindings, Ableton wire state) that must never be UI-reachable; the projection boundary is the correct place to drop it.

**D2 — Row identity is `(GraphParamTarget, ParamId)`, carried on every action; positional indexes die.** `id_to_index`, `pid_at(pi)`, and every `pi`-shaped positional coupling are deleted. Position survives only as render order, defined by the manifest walk. Rationale: positional coupling is how a forgotten mirror silently misroutes (the BUG-249/250 shape). Rejected: *a new opaque row-id scheme* — the zero-new-systems test (DESIGN_AUTHORING §3); `ParamId` + `GraphParamTarget` already address every param on the wire, and the scene panel's synthesized-id universe is the reference corpse.

**D3 — One row struct replaces fifteen parallel vecs.** `ParamRow { id, spec, value, modulation, mapping }` — descriptor (`spec`) verbatim from the manifest's `ParamSpecDef` fields, state grouped per-row. A new card fact is one field in one struct, projected in one function, rendered in one place. Rejected: *keeping `ParamCardConfig` and building `ParamSurface` beside it* — the parallel-old-path forbidden move; the DTO carries no information the manifest lacks (DESIGN_AUTHORING §4's accidental-duplication test → extract the seam, delete the copy).

**D4 — Row widgets are WidgetId-keyed by row identity.** Each row's interactive nodes are minted via the keyed builders with a salt derived from the row's `ParamId` (hash) + role discriminant, so identity survives reorder, section fold, and insertion (the `tree.rs:1747` reorder-stability contract, now at card scale). Precedent: INPUT_IDENTITY_UNIFICATION's explicit-key escape hatch, built for exactly this ("arming a modulator on one row must not renumber another row's controls"). Rejected: *auto sibling-index salts* — row insertion renumbers every later row's widgets, which is the stale-interaction class the keyed builders exist to prevent.

Two specs the fork review forced explicit (2026-07-20, BLOCKER-1/GAP-4): **card ROOTS are keyed too.** WidgetIds chain parent→child (`tree.rs:73` — a child salts off its parent's id), so ParamId-keyed rows are worthless under an auto-salted card root: dragging a card to reorder — a core gesture — would renumber every row widget in every later card. Card-root salt = stable card identity (`EffectId` hash for effect cards; the layer/generator identity for generator cards). And **the salt hasher is pinned and process-stable** (splitmix64/fx over the id bytes — never `DefaultHasher` or seeded ahash): dumps expose raw `Widget(u64)` values (`automation.rs:67`) that flow scripts may hold across runs; a run-varying hash silently breaks them.

**D5 — Gesture routing is a lookup, not a gauntlet.** During `build()` the card populates a `RowIndex: WidgetId → (row, RowRole)` from the same rows it renders — agreement by construction, the harness-seam D3-hardening precedent. Clicks resolve through it into one `row_action(surface, row, role, gesture) → PanelAction` function shared by both card kinds (the target comes off `surface.target`, killing the effect/generator twin forks). Stateless clicks may additionally ride `IntentRegistry` where they already do; the seam commitment is: **no per-widget `Option<NodeId>` field and no hand-written id-match chain survives for row elements.** Rejected: *forcing every gesture into `IntentRegistry`* — drags and drawer interactions are stateful (widget-unification D5 keeps drags out of the intent contract); the row index serves both without bending the registry.

**D6 — The live tree is the only geometry oracle.** `card_y` and `compute_height` (+ its two kind twins) are deleted; every hit-test and drag computation reads laid tree bounds. W2-B lands the drag-path fix (BUG-265); this design extends the rule to the whole card surface and pins it with the negative gate. Rejected: *updating the snapshots on the scroll path* (the findings doc's named stopgap) — keeps three geometry sources agreeing by discipline; BUG-108 and BUG-265 are the recurrence proof.

**D7 — Display-value resolution is one function, decided in the projection.** Which storage a row's shown value reads (base slot, binding slot, def fallback, driven-by-wire) is resolved when the projection builds `RowValue` — the BUG-260 class (write path works, read path lies) becomes structurally impossible because there is no second read path. The per-frame effective-value stream stays (`sync_values`), positional over the manifest order, `debug_assert!(slots.len() == rows.len())`.

**D8 — The three doctrine test families are the layer's contract, shipped with it.** (a) *Display-value resolution*: call the projection on a fixture project, assert `RowValue` — pure, GPU-free. (b) *Click→command dispatch*: build the tree headlessly, synthesize the gesture through `process_events`, assert the drained `ContentCommand` — the `undo_baseline` harness pattern, replicated never reinvented. (c) *Hit-test geometry*: pure math over laid tree bounds (UITree builds CPU-only). Pixels stay out of every gate (doctrine); the headless PNG remains a look oracle.

**D9 — Scope fence: manifest-backed param surfaces only.** In: effect cards, generator cards, editor cards, scene sections (P5, post-convergence). Out, deliberately: chrome settings sliders (master opacity et al. — few, stable, no families), macros panel (own mapping model), timeline, canvas, `PanelAction` enum reshaping beyond identity-carrying, and the 18-arg `dispatch` signature (Deferred with trigger, §10). Rationale: `feedback_dont_cascade_redesign` — the inventory (§1) decides the blast radius; the duplication class lives in the manifest-backed surfaces.

**Consequences, stated honestly:** (1) The projection is still a copy — one translation is architecturally forced by the ui↔core crate boundary, and this design does not remove it; it removes the *fourteen other* hand-maintained agreements. The copy count stays two (manifest → model → tree); the by-hand agreement count goes to ~one, pinned by a property test. (2) P1–P3 rewrite the most interaction-dense surface in the app — drags, drawers, badges, tweens. Regression risk is real and is priced in: the untouched-and-green existing card/golden/undo suites on conversion, the flow suite, and the stomp-test family are the nets, and the phases are sequenced so each lands committable. (3) Until P5, the scene panel and the cards briefly run on different vintages of the card path (convergence lands first); this is a sequencing window, not a kept parallel path — P5's gate closes it. (4) `RowRole` is a new enum that must grow when a genuinely new row affordance ships; that is the intended single place, but it is one more vocabulary to learn — the catalog comment in `param_surface.rs` is its documentation home.

---

## 3. Data model (committed signatures)

New module `crates/manifold-ui/src/param_surface.rs` (manifold-ui depends only on foundation — every type below uses foundation/ui-local vocabulary only). Load-bearing shapes committed; field partitioning interiors free where marked.

```rust
/// The complete queryable description of one manifest-backed param surface.
/// Built app-side by `ui_translate::param_surface()` (the ONE projection);
/// read by the card renderer, the gesture router, tests, and the dump.
pub struct ParamSurface {
    pub target: GraphParamTarget,   // card identity on the wire (existing type)
    pub kind: ParamCardKind,
    pub title: String,
    pub enabled: bool,
    pub collapsed: bool,
    pub rows: Vec<ParamRow>,        // manifest order == render order
    pub string_rows: Vec<ParamCardStringInfo>,   // existing type, unchanged
    pub relight: RelightCardConfig,              // existing type, unchanged
    pub audio_sends: Vec<UiAudioSendChoice>,     // card-level send list (from AudioCardState)
}

pub struct ParamRow {
    pub id: ParamId,        // THE identity: WidgetId salt, wire id, test address
    pub spec: RowSpec,      // descriptor, verbatim from the manifest ParamSpecDef
    pub value: RowValue,
    pub modulation: RowMod,
    pub mapping: RowMapping,
}

/// Descriptor — the fields ParamInfo carries today (name, min, max, default,
/// whole_numbers, is_angle, is_toggle, is_trigger, is_trigger_gate,
/// value_labels, section, exposed). Interior partitioning free; the seam is:
/// sourced ONLY from the manifest walk — no registry re-reads, no hand tables.
pub struct RowSpec { /* interior free */ }

pub struct RowValue {
    pub base: f32,          // user-intended (pre-modulation)
    pub effective: f32,     // post-modulation — what the slider shows
    pub exposed: bool,
    pub driven: bool,       // wire-fed (read-only presentation, scene D2 answer)
}

/// Driver + envelope + audio + automation state for ONE row — today's ~15
/// parallel vecs (`driver_active` … `automation_overridden`) plus the per-row
/// slice of AudioCardState, as one struct. Interior partitioning free.
pub struct RowMod { /* interior free */ }

/// Ableton display + range, OSC address, mappable flag. Interior free.
pub struct RowMapping { /* interior free */ }

/// Every interactive element a row can build, by role — the vocabulary the
/// router speaks. One enum, both card kinds. Executor extends variants as the
/// build conversion reaches each element; each variant is added ONCE here,
/// never as a new id-hoard field.
pub enum RowRole {
    SliderTrack, ValueCell, DriverBtn, EnvelopeBtn, AudioBtn, ToggleBtn,
    TrimMin, TrimMax, EnvTarget, MappingChevron, Label, SectionHeader,
    // … drawer/config roles, enumerated at P2 from the deleted id-hoard fields
}

/// Reverse map, populated during build() from the same rows being rendered.
/// WidgetId-keyed: rebuild- and reorder-stable by construction.
pub struct RowIndex {
    map: AHashMap<WidgetId, (usize, RowRole)>,
}
```

App side, in `ui_translate.rs` (or a `param_surface` sibling module — executor's call, same crate):

```rust
/// THE projection. Replaces preset_to_config + build_card_modulation +
/// build_audio_card_state. One manifest walk; display-value resolution (D7)
/// happens here and nowhere else.
pub fn param_surface(
    inst: &PresetInstance,
    kind: PresetKind,
    target: GraphParamTarget,
    osc_scope: OscScope<'_>,
    clip_string_params: Option<&BTreeMap<String, String>>,
    automation_latched: &[(EffectId, ParamId)],
) -> Option<ParamSurface>;
```

Routing, in `param_card.rs`:

```rust
/// One role→action function, both card kinds. Consumes row identity; emits
/// the SAME PanelActions the gauntlets emit today (wire unchanged).
fn row_action(surface: &ParamSurface, row: usize, role: RowRole, gesture: RowGesture)
    -> Option<PanelAction>;
```

`⚠ VERIFY-AT-IMPL (P1): the exact ParamCardConfig → ParamSurface field mapping` — re-derive the DTO's field list against current `param_card.rs:305–373` before writing `ParamRow` interiors; Wave-1.5/Wave-2 may have shifted lines. A DTO field with no home in the model is an escalation, not a silent drop.

**Who owns it / thread / serialization / mutation** (the §3 four): the UI thread owns `ParamSurface` instances (stored where `ParamCardConfig` is stored today — on the card panel); built from `Arc<Project>` snapshot data on the structural-sync path (`sync_inspector_data` cadence, NOT per-frame); never serialized; never mutated — a new projection replaces it wholesale on data change, and user gestures go through `PanelAction` dispatch exactly as today.

---

## 4. Gesture routing (the P2 seam)

Build populates `RowIndex` as it mints keyed widgets. Event handling becomes:

1. `handle_click(node_id)` → `tree.widget_of(node_id)` → `row_index.get(widget)` → `row_action(...)`. The relight/section/header specials become roles, not field matches.
2. `handle_pointer_down`/`handle_drag`: same lookup to identify `(row, role)`; the existing drag machinery (`SliderDragState`, trim handles, drawer state in `ParamCardState`) keeps its state — only *identification* changes source.
3. Card-level chrome (toggle/chevron/cog/drag-handle) stays on `register_intents`/existing paths — those are per-card, not per-row, and are not part of the disease.
4. `register_intents` (:4668–4760 — the right-click contract layer) is re-pointed at rows in P2: today it reads the id-hoards being deleted. Right-click reset/mapping stays on the widget-contract path (widget-unification D5); `RowIndex` never absorbs it.

Deleted at the end of P2 (the deletion gate proves it): every `Vec<Option<NodeId>>` row-element hoard, `match_param_row_click` and its 10 parallel arrays, `handle_click_effect`/`handle_click_generator` as separate bodies, `pid_at`, `id_to_index`.

The plausible-wrong turn, forbidden by name: **do not build a live data-binding registry** — a runtime system where widgets subscribe to model paths and sync writes through bindings. It is the generic-framework answer a hurried implementer reaches for, it is the scene panel's imitation-exposure mistake at 10× scale, and it fails the zero-new-systems test (a second dispatch system beside `PanelAction`). The row index is not that: it is a per-build reverse map of what build just created, empty of policy, rebuilt with the tree. Equally forbidden: keeping one "hard" id-hoard field "just for the weird drawer" (adapter/parallel-path), and any `is_harness`/test-only branch in the projection or router.

---

## 5. Queryability (what the layer answers, for whom)

| Consumer | Query | Mechanism |
|---|---|---|
| Renderer | rows + state for a surface | `ParamSurface` (P1) |
| Gesture router | widget → (row, role) | `RowIndex` (P2) |
| State tests | shown value for a param | projection call, pure (P1) |
| State tests | command a gesture produces | headless tree + `process_events` + drain (P2) |
| State tests | hit geometry | laid tree bounds, pure math (P3) |
| Automation flows / agents | address a row by param | keyed WidgetIds make row widgets durable in the dump; `SelectorQuery`/`Widget(u64)` resolve them (P2, free) |
| The dump | row identity legible | row-root nodes carry the param id as node name — `⚠ VERIFY-AT-IMPL (P2): confirm dump serializes node names for these nodes; wire if not` |

No new query *protocol* is invented: tests call functions, flows use the existing selector DOM. That is the point.

---

## 5b. Agent contract & enforcement (added 2026-07-20, adversarial review — Peter's directive)

Peter's rule (quoted in the intro) is standing: **agents never build their own infra for rows, sliders, drawers.** Prose alone provably fails here — the scene panel was built against explicit design-doc prohibitions. The mechanisms that actually stop lanes in this repo are invariant tests and hooks; this section ships both.

**Sanctioned entry points** — the only ways to put a row-shaped control on screen:

| Need | The one entry point |
|---|---|
| Manifest-backed param rows, any panel | this layer: project a `ParamSurface`, embed the card row host (the scene-convergence precedent — panel rows ARE card rows) |
| A plain labeled slider in panel chrome | `View::slider_row(SliderSpec)` / `param_slider_shared` builders |
| A drawer/config sub-surface on a row | the row's `RowRole` + the existing drawer machinery, extended per the recipe below |
| Anything row-shaped these don't cover | STOP and report up — "existing system doesn't cover X" is a report, never a license to build (operating model, `SYSTEM_UPGRADE_2026_07_PLAN.md`) |

**Enforcement, layered:**

1. **`no_bespoke_row_infra` (INV-8):** repo-wide invariant test — an allowlist scan over `crates/manifold-ui/src/panels/**` in which raw `BitmapSlider` construction and row-routing id collections outside the sanctioned modules **fail `cargo nextest`, not review**. Shipped in P2; the allowlist is the table above, in code.
2. **Hook deny-pattern:** a PreToolUse edit-guard flagging new `BitmapSlider::new` / row-hoard patterns outside the allowlist, advisory-prompt severity (the dead-code-suppression hook is the precedent). Rides the approved hook-trim pass (`3f2dfc0a` is the vehicle; mechanics live there, not here).
3. **The recipe** (agent-facing; lives as `param_surface.rs`'s module doc, and every card-touching brief points at it). To add a row affordance: (1) add the `RowRole` variant; (2) add the fact as ONE field on `ParamRow` + its projection line; (3) add the render arm in the row builder; (4) add the `row_action` arm; (5) add the dispatch test (Harness pattern). **Five steps, never five files** — anything that can't be expressed this way is an escalation, by definition of the layer.
4. **CLAUDE.md** gains one line at P2 landing pointing here (the rule, not the story, per the rewrite doctrine).

---

## 6. Invariants & enforcement

| Invariant | Enforcement (machine check, by name) |
|---|---|
| INV-1 Rows mirror the manifest — every manifest slot has exactly one row, ids equal, order equal | `param_surface_tests::rows_mirror_manifest` property test over fixture projects incl. the Liveschool canonical fixture (P1 deliverable) |
| INV-2 Actions carry identity, never positions | deletion of `pid_at`/`id_to_index` (`rg 'pid_at|id_to_index' crates/manifold-ui crates/manifold-app` → 0, P2 gate); dispatch tests assert emitted actions carry the clicked row's `ParamId` |
| INV-3 The tree is the only *post-build* geometry source (restated 2026-07-20 post-W2-B: `card_y` already deleted; `compute_height` survives as `build()`'s layout cursor, which IS the geometry source at build time) | `rg 'card_y' crates/manifold-ui/src/panels` → 0 (true since `94632d65`); `compute_height` call sites confined to `build()`/column-height helpers — reviewed at P3 landing with the call-site list in the landing report; geometry tests compute drop targets from tree bounds under scroll + mid-tween states (P3 gate) |
| INV-4 Widget identity survives ROW reorder/insert/fold AND CARD reorder, and is process-stable | keyed-identity tests shaped like `tree.rs:1747` covering both row reorder and card reorder (the D4 card-root spec), plus a pinned-hash test (known id bytes → known salt, cross-run) (P2 deliverables) |
| INV-5 No id storage used for ROUTING — `handle_*` bodies contain no id-equality matching against stored node-id collections; widget-owned id bundles (`SliderIds`, `ToggleParamIds`) are exempt widget state | P2 gate: `rg 'match_param_row_click|handle_click_effect|handle_click_generator' crates/manifold-ui` → 0, plus the exempt-type list named in the phase report. (A blunt `Vec<Option<NodeId>>` scan alone is both too narrow — misses `Vec<Option<SliderIds>>` bundles — and wrong-headed — flags legitimate widget state.) |
| INV-6 Per-frame value stream matches rows | `debug_assert!` in dev; in release a length mismatch SKIPS the sync and logs loudly once — never a silent positional miswrite (`no-silent-fallbacks`) (P1) |
| INV-8 No bespoke row infra anywhere, ever (Peter's standing rule, §5b) | `no_bespoke_row_infra` repo-wide allowlist test (P2 deliverable) + the §5b hook deny-pattern |
| INV-7 One display-value read path | BUG-260-shaped conviction test binds the projection's `RowValue` to the binding-slot read for bound params (P1 deliverable, pattern: `bound_row_display_reads_the_binding_slot_not_the_def`) |

---

## 7. Phasing

Phased by **layer**, never by family (DESIGN_AUTHORING §7): P1 swaps the data, P2 the routing, P3 the geometry, P4 the bridge, P5 flows the remaining surfaces through. Every phase ends committable with the old code it replaces deleted. Test scope per phase: focused nextest + `-p` clippy; single full sweep at each landing in the main checkout. No GPU tests anywhere (nothing touches kernels); headless PNGs are look-oracles only.

### P1 — Model swap, split at the fork review (GAP-3: "one session" was false — two cliffs named)

**P1a — collapse the parallel vecs in place (mechanical).** Inside the EXISTING `ParamCardConfig`: the ~15 per-param vecs PLUS `AudioCardState`'s ~14 more (`param_slider_shared.rs:572–600` — the fork-found second cliff) become per-row structs (`RowMod`-shaped), and `ParamModState`'s sync paths (`sync_audio` et al.) are re-pointed at them. No new types cross a crate boundary; no parallel path (the DTO's interior changes, its name and consumers don't).

- **Entry:** `rg -n 'struct ParamCardConfig' crates/manifold-ui/src/panels/param_card.rs` hits; `rg -n 'struct AudioCardState' crates/manifold-ui/src/panels/param_slider_shared.rs` hits; re-derive counts.
- **Gate:** existing card/golden/undo suites green untouched; negative: `rg 'driver_active|trim_min|target_norm|env_decay' crates/manifold-ui` → 0 (the vec names are gone).
- **Demo:** none — **L1**, honestly (interior refactor pinned by untouched suites).

**P1b — swap to `ParamSurface` + the one projection (vertical slice; Fable-grade per AGENT_ROUTING — this is the judgment phase).**

- **Entry:** P1a landed; `rg -n 'fn preset_to_config' crates/manifold-app/src/ui_bridge/state_sync.rs` hits; re-anchor (Wave-2 lands in parallel).
- **Read-back:** this doc §2–§3, `param_card.rs` config/build heads, `state_sync.rs:3043–3299`, PARAM_STORAGE_BOUNDARIES D4/D12. Restate D1/D2/D3/D7, forbidden moves, entry findings.
- **Deliverables:** `param_surface.rs` types; `param_surface()` projection (absorbing `preset_to_config`/`build_card_modulation`/`build_audio_card_state`); `configure()`/`build()`/`sync_values()` re-pointed at rows; editor card assembly re-pointed **including an equality/hash story for `editor_card_config_hash` (`app.rs:798`) — `ParamSurface` derives `PartialEq` or ships a content hash, or the editor rebuilds every frame (fork-found cliff, named deliverable)**; `ParamCardConfig`+`ParamInfo` deleted (compiler-driven); INV-1/INV-6/INV-7 checks.
- **Gate (positive):** existing card/golden tests green untouched; `rows_mirror_manifest` green incl. Liveschool fixture; both shipped ui-flows exit 0; full-app + editor headless PNGs produced and read (unchanged layout). **Gate (negative):** `rg 'ParamCardConfig|ParamInfo\b|id_to_index' crates/` → only history/docs hits.
- **Acceptance demo:** the two PNGs + flow exit codes. **L3** (flows drive the real input path). **Performer gesture:** scrub a layer-effect slider — value follows, one undo entry (the `undo_baseline` suite is the behavior contract for this swap).
- **Forbidden moves:** keeping `ParamCardConfig` alive "for the editor path" · any second projection function per kind/surface · reading the registry inside the projection where the manifest already carries the fact · touching click routing (that is P2).

### P2 — Routing swap: row index + one action function

- **Entry:** P1 landed; `rg -n 'match_param_row_click' crates/manifold-ui` hits; enumerate every `Vec<Option<NodeId>>` field in `param_card.rs` (the role inventory — write it into the phase notes).
- **Read-back:** §4, §5, the `handle_click*` bodies, `docs/archive/INPUT_IDENTITY_UNIFICATION.md`, widget-unification D5. Restate D4/D5 and the data-binding-registry prohibition.
- **Deliverables:** keyed row-widget minting (`ParamId`-derived salts) AND keyed card-root minting (D4's card-identity salts); `RowIndex`; `row_action()`; `handle_click`/`handle_pointer_down`/`handle_drag` re-pointed; **`register_intents` re-pointed at rows — right-click reset/mapping stays on the widget-contract path, never absorbed into `RowIndex` (GAP-2)**; deletion of the gauntlets + routing hoards + `pid_at`; INV-2/INV-4/INV-5 checks + the pinned-hash test; **`no_bespoke_row_infra` (INV-8) + the §5b recipe as `param_surface.rs`'s module doc + the CLAUDE.md pointer line**; the click→command dispatch test family seeded (one test per `RowRole` variant, `Harness` pattern) — this is W2-A's pattern library, coordinate so tests live once.
- **Gate (positive):** dispatch tests green (every role emits the same `PanelAction` the old gauntlet emitted — enumerated from the P2 role inventory, count-matched, not a chosen subset); reorder-stability test green; flows green. **Gate (negative):** INV-2/INV-5 rg gates → 0.
- **Acceptance demo:** an L3 flow that clicks D, E, and A on a named row by selector and asserts the drawer state + dispatched command. **Performer gesture:** arm a driver on a layer effect mid-playback — badge lights, drawer opens, undo removes it cleanly.
- **Forbidden moves:** a surviving id-hoard "just for X" · routing drags through `IntentRegistry` · inventing a new selector protocol (the existing DOM resolves keyed widgets already).

### P3 — Geometry monopoly

> **Addendum 2026-07-20 (post-W2-B, re-entry session):** W2-B landed the same day this doc was authored (`f2ac71d9`), and its landing fixup (`94632d65`) already deleted `card_y`. The entry condition is satisfied and the deliverable shrinks: what remains is *proving the monopoly*, not winning it — `compute_height` survives legitimately as `build()`'s layout cursor and the column-height helpers (re-verified: all ~20 call sites are build-path), and the class-pin is the test family plus the restated INV-3 below.

- **Entry:** W2-B (BUG-265) landed — verify `git log --grep BUG-265` shows the root fix (expected: `f2ac71d9`); verify `rg 'card_y' crates/manifold-ui/src/panels` → 0 (expected already true).
- **Read-back:** INSPECTOR_DRAG_TAB_FINDINGS (all three root causes), W2-B's landed diff. Restate D6 and the addendum above.
- **Deliverables:** an audit pass proving `compute_height` has no post-build consumer (any found is re-pointed at laid bounds or escalated); geometry test family (drop-target math under scroll offsets and mid-tween heights, pure — the BUG-265/BUG-108 repro as math); the INV-3 gate wired.
- **Gate:** INV-3 as restated; geometry tests green; drag flow (the `drag-clip.json` pattern, card variant) exits 0. **Acceptance demo:** the L3 drag flow. **Performer gesture:** scroll the inspector, then drag a card — the indicator lands where the cursor is (BUG-265's exact complaint).
- **Forbidden moves:** updating snapshots instead of deleting them (the findings doc's named stopgap) · asserting pixels for geometry · deleting `compute_height`'s build-time layout role for the sake of a cleaner rg gate (build-time placement IS the layout pass; the disease was post-build reads).

### P4 — Bridge consolidation

- **Entry:** P2 landed. Verify whether convergence has deleted the scene funnels (`rg 'resolve_scene_write|scene_bound_slot' crates/manifold-app` — if still present, they are the lane's to delete, not this phase's).
- **Read-back:** `ui_bridge/inspector.rs:62–520`, the exemplar test mods. Restate D2 and the scope fence.
- **Deliverables:** the effect/generator dual-edit twins in `ui_bridge/inspector.rs` (:62–223) collapsed over `GraphParamTarget`; param action handlers verified one-per-action-kind (no per-family copies); `resolve_param_range` re-pointed at the projection's spec if still hand-resolving.
- **Gate:** `undo_baseline` + `mapping_undo_baseline` + `bug_266_tab_pin` suites green untouched; focused nextest on `manifold-app`; negative: no `ParamCardKind`/kind fork remains in param dispatch paths (`rg 'ParamCardKind::' crates/manifold-app/src/ui_bridge` → projection only). **Acceptance demo:** none — **L1**, stated honestly (pure consolidation; behavior pinned by the untouched baselines).
- **Forbidden moves:** touching the 18-arg `dispatch` signature (Deferred) · "improving" non-param dispatch while in the file.

### P5 — Remaining surfaces + flow sweep + supersession

- **Entry:** scene convergence P2+ landed (scene rows are card rows). Re-verify with `git log` + the convergence doc's status line.
- **Deliverables:** scene sections and editor cards confirmed riding the identical host (they should by construction — this phase *proves* N=3 surfaces, adds no code unless a divergence surfaces, which escalates); the scene/card flow suites re-pointed where selectors changed; supersession sweep (this doc's status line; `rg` for `ParamCardConfig`, `match_param_row_click`, and this design's stage labels across docs/ + the memory directory; fix or tombstone every stale hit); `VERIFICATION_DEBT.md` entries for any gap.
- **Gate:** flow accounting is a **count match** over every flow file on disk for the inspector/scene/editor surfaces (the BUG-252 rule), not a named subset. **Acceptance demo:** **L3** — one flow per surface driving a row gesture end-to-end.
- **Forbidden moves:** "the scene panel mostly works" (a divergence is an escalation, not a shim) · leaving any prior scene/card design doc claiming ownership of machinery this design deleted.

---

## 8. Coordination (live workstreams this touches)

- **Wave 2 (parallel session):** W2-B **landed 2026-07-20 (`f2ac71d9` + `94632d65`)** — P3's prerequisite is satisfied. W2-A test families — P2 seeds the same patterns; whoever lands second rebases onto the other's test module, no duplicate harness. W2-C's drag survey findings (lane in flight) may extend the geometry test family's cases.
- **Scene convergence (lane, in flight):** P5 consumes its end state. Nothing in P1–P4 touches scene files.
- **param-descriptor-unification brief (pending, memory handoff):** independent — it unifies the *registry-side* descriptor twins; this design reads the manifest either way. If it lands first, `RowSpec`'s source comment names `ParamSpecDef` directly; if not, nothing here blocks.
- **Trigger-drawer redesign / editor-window unification (intake handoffs):** both become consumers — their briefs should target `RowRole`/projection extensions, not new machinery. Flag at briefing time.
- **God-file decomposition (plan §Later):** follows P1–P4 — splitting `param_card.rs`/`state_sync.rs` before the duplication dies just spreads the mess (the plan's own sequencing rule). After P4 the files are smaller and the split is mechanical.

## 9. Decided — do not reopen

1. One projection function for all manifest-backed surfaces; app-side; UI-thread; never serialized; never mutated in place. (D1)
2. Row identity = `(GraphParamTarget, ParamId)`; positional coupling deleted. No new id scheme, ever (the scene panel is the corpse). (D2)
3. `ParamRow` replaces the parallel vecs; a new card fact = one field, one projection site, one render site. (D3)
4. Row widgets are keyed by param-derived salts AND card roots by card-identity salts, through a pinned, process-stable hasher; identity survives row reorder, card reorder, and process restarts. (D4)
5. Routing = `RowIndex` lookup + one `row_action`; no id-hoard fields, no kind-forked click bodies, and NO live data-binding registry. (D5)
6. The live tree is the only geometry source; snapshots are deleted, not maintained. (D6)
7. Display-value resolution happens once, in the projection. (D7)
8. Gates are state-level; pixels are look-oracles, never asserted. (D8)
9. Scope: manifest-backed param surfaces only. (D9)
10. `ParamCardState` (drag/drawer/tween runtime) stays imperative — the chrome-declarative/widget-imperative split is the architecture.
11. Peter's standing rule (§5b): agents never build bespoke row/slider/drawer infra; the sanctioned entry points are the only doors, enforced by `no_bespoke_row_infra` + the hook deny-pattern, not by review. Right-click contracts stay on the widget-contract path.

## 10. Deferred (with revival triggers)

- **`dispatch`'s 18-arg signature → a context struct.** Revive when the next new snapshot-slot argument is added (the 19th arg is the trigger), or when god-file decomposition of `ui_bridge` begins. Not duplication, just ugliness — outside this design's class.
- **`PanelAction` god-enum decomposition.** Revive with the god-file decomposition wave that FOLLOWS this design. The wire stays stable through P1–P5 deliberately — one moving layer at a time.
- **Macros panel / settings sliders onto the layer.** Revive if either grows families or ships a duplication bug; today they are small and stable.
- **A unified scrub wire + gesture engine (`PanelAction::Scrub(ValueRef, ScrubPhase)`).** Independently derived and priced at the 2026-07-20 re-entry review: collapse the ~17 per-family Snapshot/Changed/Commit variant trios, the 8 parallel `&mut Option<…>` snapshot slots in `dispatch()` (ui_bridge/mod.rs:166–176), and `ActiveInspectorDrag`'s per-family variants into one address enum (`ActiveInspectorDrag` at app.rs:52 is the shape precedent — the restore path already forced it into existence) with one `read`/`apply`/`live_command`/`commit_command` implementation per address. Deliberately NOT in this design's scope: the observed 2026-07 bug class lives in the manifest-backed surfaces and geometry, not in the settings/macro trios (each is a sole implementation — the UI_WIDGET_UNIFICATION D18 test), and the one real guard-family bug (the `SceneParam` silent-no-op restore) dies with convergence. Revive on the next new scrub family, the next guard/snapshot-slot bug, or the `ui_bridge` decomposition wave — the `undo_baseline` suite is the ready-made parity oracle when it happens.
- **`param_slots_to_ui`'s per-frame Vec allocation → scratch buffer.** Pre-existing, UI-thread (not the content hot path); revive on the next UI-frame-cost investigation or when a profiler run names it.
- **Automation flow verbs that set params directly ("set X to 0.5" without a drag).** Revive when a flow needs it; the selector + keyed rows make it cheap then. Not needed for this design's gates.
- **Widget catalog / runtime self-inspection beyond the dump** (overhaul §10.2's remainder). The dump + selector + this layer cover today's agent authoring; revive when an agent workflow demonstrably wants more.
