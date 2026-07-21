# UI Funnel Decomposition — Wave 1 of the god-file campaign

**Status: IN PROGRESS — P-P/P-B/P-F/P-D SHIPPED; P-I (scrub wire) SHIPPED 2026-07-22 (landing report `docs/landings/2026-07-22-ui-funnel-p-i.md`; ActiveInspectorDrag extinct, D8 verb live); remaining: P-S (split-by-layer + RowHost dedup per census), P-Z (close) · 2026-07-22 · Fable**
**Prerequisites:** WIDGET_TREE (COMPLETE 2026-07-21), SCENE_PANEL_EXPOSURE_CONVERGENCE (COMPLETE 2026-07-21). Campaign register: `docs/ARCHITECTURE_DEBT.md` (inventory + wave map; status for this wave lives ONLY on this doc's Status line).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase. Pure-move commits gate on `scripts/move_identity_check.py` (built + self-tested 2026-07-21).

**The governing insight: the funnel files are not big because the UI is big — they are big because four concerns (projecting state, describing surfaces, routing gestures, translating to commands) each live a slice in every domain's file instead of each owning a thin layer.** Every UI change funnels through `dispatch` (18 args, 303-variant match), `dispatch_inspector` (one 3,160-line function), `sync_inspector_data`/`push_state`, and `tick_and_render` (one 3,270-line frame function). The end state is the matrix: **layers are the hard boundaries, domains are small files within each layer.** Payoff test, honest form: adding a new panel touches nothing outside its own domain plus one registration line per registry.

Peter's directives, verbatim (2026-07-21): *"no deferred-with-revival-triggers. We design to prevent bug classes, not to wait for the next failure"* — every WIDGET_TREE §10 item is designed in below (§2 D8–D10); *"It is the fundamental app infrastructure, so no bandaids, no cheap hacks, no lazy work"*; *"I want god files gone, I want proper software architecture, designs, and boundaries."*

Stage translation: none of this changes pixels. It changes how fast and how safely everything else lands between now and the ~Aug release: a scrub bug becomes a bug in the scrub module, not a hunt across 29k lines; a new performance surface becomes an afternoon (projection file + intent enum + manifest + registration), not a wave.

**Binding constraints** (DESIGN_AUTHORING §1): *Hot path* — `push_state`/`sync_card_values` run per-frame; the split must not move structural work onto the per-frame path (INV-W4). *Thread residency* — untouched: UI thread owns everything here; mutations stay `PanelAction` → dispatch → `ContentCommand`. *Time model* — untouched. *Persistence* — none of the moved types serialize; the fixture round-trip gate is a belt-and-braces net. *Performance surface* — every dispatch path IS the live-control path; the flow suite + `undo_baseline` family are the behavior oracle.

Companion docs: `WIDGET_TREE_DESIGN.md` (the layer vocabulary this extends; its §10 Deferred is absorbed and closed by this doc), `docs/ARCHITECTURE_DEBT.md` (campaign register), `AGENT_ROUTING.md` (orchestration), `.claude/GIT_TREE_DISCIPLINE.md` (landing mechanics).

---

## 1. Audit — what exists (verified 2026-07-21, this session)

| Piece | Where | State |
|---|---|---|
| `dispatch` — single action entry point | `ui_bridge/mod.rs:158–698`; 18 args: 7 context, 8 `&mut Option<…>` scrub-baseline slots, `user_prefs`, `active_inspector_drag`, `editor_target`; `SliderReset` recursion re-threads all 18 (:196) | SPLIT (P-B) + slots DIE with the scrub wire (P-I) |
| Domain sub-dispatchers | `ui_bridge/{editing,layer,marker,project,transport}.rs` — dispatch already delegates per domain (variant mentions: 24/23/7/48/26) | EXISTS — the domain decomposition half-done; extend, don't redesign |
| `dispatch_inspector` | `ui_bridge/inspector.rs:441–~3600` — ONE function, ~3,160 lines, 150 distinct `PanelAction` variants; dual-edit helpers :61–223; target resolvers :223–341 | SPLIT into per-domain handler modules (P-B, pure moves) |
| Inspector test corpus | `ui_bridge/inspector.rs:3603–6012` (~2,400 lines) incl. `undo_baseline` (:3880 area), `mapping_undo_baseline`, `bug_266_tab_pin` — `Harness` over a real `EditingService` | MOVES with its code (P-B); the scrub-wire parity oracle (P-I) |
| `state_sync.rs` | 4,177 lines: `push_state` :234–879 (per-frame), `sync_project_data` :1136, `sync_clip_positions` :1443, `sync_inspector_data` :1570–2715 (~1,150-line fn), `param_surface` projection :3184 (widget-tree P1b), modulation builders :2747–3000 | SPLIT into `projection/` per-domain modules (P-P, pure moves) |
| Scrub-gesture state | `app.rs:525–573` — 10 snapshot fields on `Application` mirroring dispatch's slots (incl. `mapping_range_snapshot`/`mapping_affine_snapshot`, NOT in dispatch's args); `ActiveInspectorDrag` `app.rs:52–420` (per-family variants + `apply()` restore path); `BoundNodeParamDrag`/`UnboundNodeParamDrag` `app_render.rs:287–331` (editor-side twins) | DIE into `ScrubState` + `ScrubAddress` (P-I) |
| Scrub variant trios | `panels/mod.rs` — 96 Snapshot/Changed/Commit-suffixed hits across the 303-variant `PanelAction` (:179–1158), ≈30 trios | COLLAPSE to `Scrub(ValueRef, ScrubPhase)` (P-I) |
| `tick_and_render` | `app_render.rs:839–4110` (~3,270 lines): state drain :850–1220 → event pump + dispatch :1228–3738 → structural sync :3584–3650 → `push_state` :3780 → present :4058 | SPLIT into `frame/` stage modules (P-F, pure moves) |
| Graph-editor bridge cluster | `app_render.rs:26–830` — mapping build/commit, `watched_*` reads, drag structs; plus `present_graph_editor_window` :4141–4876 | MOVES to `editor_bridge.rs` / `frame/present.rs` (P-F) |
| `Application` god-struct | `app.rs:421–950` (~530 lines of fields: GPU, windows, content channels, import workers, breadcrumbs, selection, scrub, clipboard, rendering); inline WGSL :2552–2660 | Field GROUPS extracted with their subsystems (P-F); scrub fields die in P-I |
| `ui_root.rs` | 4,080 lines: `UIRoot` :177–411, 107 methods :411–3343, overlay/drag-owner machinery, dropdown builders | SPLIT: panel wiring vs overlay/drag vs dropdown builders (P-F) |
| `param_card.rs` | 6,946 lines: `ParamCardPanel` impl :743–4583 (build/render/route/drag in one impl), `ParamCardState`, relight config, row geometry | MIGRATE-THEN-SPLIT (P-S): renderer vs routing vs runtime state; bespoke remainder migrates into `param_surface` machinery |
| `scene_setup_panel.rs` + `panels/inspector.rs` + `param_slider_shared.rs` | 3,584 / 4,231 / 3,166 — VM types, outliner builders, column layout, shared slider builders | P-S consumers; split along the same layer lines |
| Widget-tree layer | `param_surface.rs` (ParamSurface/ParamRow/RowIndex/row_action), INV-1..8, `no_bespoke_row_infra` | EXISTS — the vocabulary and enforcement this wave extends |
| Flow suite | 40 flows `scripts/ui-flows/`, selector-state asserts (no pixels) | EXISTS — behavior oracle; coverage enumerated per phase (BUG-252 count-match rule) |
| Move-identity verifier | `scripts/move_identity_check.py` — pinned-color `--color-moved` parse, zero-residue gate; self-tested (pure move → 0, smuggled edit → 1) | BUILT this session |
| `cargo public-api` | not installed; `manifold-app` has no lib target | REJECTED as gate (adversarial review HIGH-1) |

Classification: **exists** — domain sub-dispatchers, widget-tree layer + enforcement, Harness/undo_baseline oracles, flow suite, `ActiveInspectorDrag` as the scrub-address shape precedent, `View::identity`/keyed builders. **One wire away** — `dispatch` already delegates per domain (inspector is just the domain that never got split); `state_sync` already has per-concern functions (they just share one file); `param_surface()` is already THE projection for cards. **Genuinely new** — `DispatchCtx`, `ScrubState`/`ScrubAddress`/`ValueRef`, per-domain intent enums, the `frame/` stage seams, the regrowth invariant test. No new identity/addressing/dispatch systems: `ValueRef` reuses `GraphParamTarget`+`ParamId` addressing (D2 of widget-tree), intents reuse the existing delegate seams. Zero-new-systems test: passes.

Negative claims, checked: no existing context struct for dispatch (`rg 'DispatchCtx|DispatchContext' crates/` → 0); no existing frame-stage abstraction in `manifold-app` (`rg 'trait.*FrameStage|mod frame' crates/manifold-app/src` → 0).

---

## 2. Decisions

**D1 — Matrix architecture: layers own modules, domains own files within them.** `ui_bridge/` becomes `projection/` (per-domain `&Project` → view-model), `dispatch/` (per-domain intent handlers), `scrub.rs` (the one gesture engine), `context.rs` (`DispatchCtx`). `app_render.rs` becomes `frame/` (stage modules) + `editor_bridge.rs`. Rejected: *split by domain only* (inspector.rs → smaller inspector files, concerns still mixed) — that is how the god files grew; it shrinks files without removing the every-change-funnels-through-everything property. Rejected: *new crates now* — module seams with `pub(crate)` first; crate promotion is a later, per-seam judgment once seams have proven one-directional (register records candidates; avoids serde/import churn mid-wave).

**D2 — Layer vocabulary is fixed, campaign-wide:** **Projection** (read: snapshot → view-model), **Surface** (describe: manifests/VMs → tree, the widget-tree layer), **Routing** (intend: gesture → typed intent), **Bridge** (translate: intent → `ContentCommand`/`EditingService`), **Frame** (orchestrate: drain → events → sync → push → present), **Geometry** (laid tree bounds, already monopolized by widget-tree D6). Every extracted module maps to exactly one. The register carries this table; parallel lanes never invent names.

**D3 — `DispatchCtx` carries dispatch's read-context; scrub state is NOT in it.** Committed shape (P-B):

```rust
// ui_bridge/context.rs
pub struct DispatchCtx<'a> {
    pub project: &'a mut Project,             // local snapshot for immediate feedback
    pub content_tx: &'a Sender<ContentCommand>,
    pub content_state: &'a ContentState,
    pub ui: &'a mut UIRoot,
    pub selection: &'a mut SelectionState,
    pub active_layer: &'a mut Option<LayerId>,
    pub user_prefs: &'a mut UserPrefs,
    pub editor_target: Option<&'a GraphTarget>,
    pub scrub: &'a mut ScrubState,            // P-I; one field, not ten
}
pub fn dispatch(action: &PanelAction, ctx: &mut DispatchCtx) -> DispatchResult;
```

Per-arg partition of today's 18 (adversarial review HIGH-2 — one owner per arg): args 1–7 + `user_prefs` + `editor_target` → `DispatchCtx` at P-B; the 8 snapshot slots + `active_inspector_drag` → **die at P-I**, interim carried as one `scrub: &mut ScrubState` field holding today's `Option`s verbatim (P-B moves them off `Application`'s field list into the struct WITHOUT reshaping them — pure mechanical regroup; P-I reshapes). Rejected: *snapshot slots as projections* — they are `&mut` in-flight gesture state, not reads (the review caught this exact misassignment).

**D4 — The scrub wire: one gesture engine, addresses not families.** Committed shapes (P-I; the WIDGET_TREE §10 priced design, promoted):

```rust
// ui_bridge/scrub.rs
pub enum ScrubPhase { Begin, Move, Commit }   // maps 1:1 onto today's Snapshot/Changed/Commit
pub enum ValueRef { /* one variant per scrubable address family — enumerated from the
    ~30 trios + ActiveInspectorDrag variants + the 2 mapping-range snapshots + the 2
    editor drag structs; re-derivation: rg 'Snapshot|DragBegin' panels/mod.rs.
    Addressing vocabulary REUSED: GraphParamTarget, ParamId, LayerId, AudioSendId —
    no new id scheme (widget-tree D2). */ }
pub struct ScrubState { /* interior free: active ValueRef + captured baseline */ }
// One impl per ValueRef variant, four operations — the ActiveInspectorDrag::apply()
// precedent (app.rs:52) generalized:
//   read(&Project, &ValueRef) -> Baseline
//   apply(&mut Project, &ValueRef, f32/shape)      // immediate local feedback
//   live_command(&ValueRef, value) -> Option<ContentCommand>
//   commit_command(&ValueRef, baseline, value) -> ContentCommand  // ONE undo entry
```

`PanelAction::Scrub(ValueRef, ScrubPhase, ScrubValue)` replaces the ~30 Snapshot/Changed/Commit trios; the 10 `Application` snapshot fields, `ActiveInspectorDrag`'s per-family variants, and the two editor drag structs collapse into `ScrubState`. `SliderReset` becomes three `Scrub` dispatches (same recursion, one-line calls). **Parity oracle: the `undo_baseline` + `mapping_undo_baseline` suites run UNMODIFIED against the new wire** — same commands drained, one undo entry per gesture — plus the existing scrub flows. Rejected: *a generic gesture framework with widget subscriptions* — the data-binding-registry prohibition (widget-tree §4) applies verbatim; `ValueRef` is an address enum, not a binding system.

**D4 — AMENDED at P-I execution (2026-07-22, ws1-scrub seats, team-lead approved).** The wire is a TWO-ROLE split, not the single four-op impl first sketched: (1) `ValueRef` (manifold-ui, wire role) — ui-relative ADDRESS only (GraphParamTarget/ParamId/LayerId/AudioSendId; no core types, no new id scheme); the value rides `ScrubValue` on Move ONLY (Begin reads baseline from the model, Commit reads final back); `ScrubValue` grew `Range(min,max)` beside `Scalar`. (2) `ResolvedScrub` (manifold-app, resolved role) — one variant per family holding undo `baseline` + resolved restore `live`, RESOLVED ONCE at Begin into the single `ScrubState.active`; `restore_dragged` re-stamps with only `&mut Project`, which is why resolution cannot be re-derived at restore. **Frame-resident families stay OFF the wire** (graph-editor mapping range/affine, graph-canvas bound/unbound node-param — the mapping commit reads back via `watched_reshape`, and wiring it would force editor watch context into `DispatchCtx`, the rejected cascade-redesign); only their stomp guards fold into `ScrubState.active`. "One gesture engine" = one restore SLOT, not one dispatch path — honest cost: two entry points, guarded by the machine-checked single-active invariant (`check_single_active_on_begin`: debug_assert + release warn on Begin-when-already-Some) at BOTH. **Wire carries what the panel knows** (crossover precedent, generalized): AudioCrossover sends a single-band Scalar (panel can't run the clamp), AbletonMacroTrim sends a Range (panel computes both edges) — shape follows the emitting panel's knowledge, never a fixed rule. Type-in commits stay one-shot exact-value commands (not gestures); the Param Begin arm's touch-to-select fires once per touch (preserved behavior, now also on type-in — observed change, accepted). D8's `AutomationAction::SetParam` rides the real wire (shipped with P-I).

**D5 — `PanelAction` decomposes into per-domain intent enums; the wire type stays one enum.** `PanelAction` becomes a thin sum: `Transport(TransportAction) | Editing(EditingAction) | Layer(LayerAction) | Marker(MarkerAction) | Project(ProjectAction) | Inspector(InspectorAction) | Scrub(...)` — matching the EXISTING delegate seams (audit: the five domain files + inspector). `InspectorAction` (150 variants today, minus ~everything the scrub wire absorbs) sub-groups by handler module (D6). Closed sets, exhaustive matches — adding an interaction extends one small enum and the compiler lists every site. The UI→bridge boundary keeps ONE action type (no signature churn at call sites; `From` impls per domain enum). Rejected: *N separate wire types* — churns every emit site for zero class-prevention gain. Rejected: *reshaping variants while decomposing* — P-D is a pure re-parenting (variant bodies verbatim); semantic reshaping is only the scrub wire, its own phase. AMENDED at P-D execution (2026-07-22, team-lead ruling): the sum is FLAT — one arm per domain (Transport, Editing, Layer, Marker, Project, Browser, Clip, Params, Modulation, Mapping, AudioSetup, Root) with `RootAction` holding mod.rs's inline-arm variants including the scrub trios until P-I adds `Scrub` as its own arm. The nested `Inspector(InspectorAction)` wrapper is REJECTED as re-encoding the funnel topology; the P-B chain router and its chain-completeness invariant retire with it, superseded by exhaustive compiler routing.

**D6 — `dispatch_inspector` splits by handler domain, speaking today's `PanelAction`, BEFORE the enum decomposes.** (Adversarial review HIGH-3's order.) Target modules under `ui_bridge/dispatch/`: `params.rs` (value edits, trims, toggles), `modulation.rs` (drivers/envelopes/audio-mod), `mapping.rs` (Ableton/macro/MIDI/OSC), `audio_setup.rs`, `browser.rs`, `scene.rs`, `clip.rs` — exact partition of the 150 variants is a P-B entry deliverable (the match-arm census, mechanical, from `rg 'PanelAction::' inspector.rs`); the resolvers (:223–341) and dual-edit helpers (:61–223) go to `dispatch/resolve.rs` shared by handlers. Pure moves throughout: bodies verbatim, `move_identity_check` zero-residue. Sub-dispatchers keep their arms verbatim behind one `match action` each with the existing `unhandled` sentinel; the router is an ordered first-non-unhandled chain — NO per-variant delegation arms, forbidden by name (a hand-written variant→module arm table is a parallel copy of the routing and where a misroute would hide).

**D7 — `frame/` stage modules with an orchestrator `tick_and_render` under one page.** `frame/drain.rs` (content-state drain + import/autosave ticks), `frame/events.rs` (pump + action collection + dispatch loop), `frame/sync.rs` (structural sync calls), `frame/push.rs` (per-frame value push), `frame/present.rs` (`present_all_windows`, `represent_cached_offscreen`), `editor_bridge.rs` (:26–830 cluster + `present_graph_editor_window`). `Application` keeps ownership; stages take `&mut Application` initially (pure move), field-group extraction (import worker state → `import.rs` struct, breadcrumb state → existing module, etc.) follows as mechanical regroup commits. Inline WGSL moves beside its pipeline. Rejected: *a FrameStage trait* — three call sites don't need dynamic dispatch; a trait here is invented infra.

**D8 — Direct-set flow verbs ride the scrub wire (WIDGET_TREE §10 item 6, designed in).** A flow verb `SetParam { selector/value-ref, value }` that emits `Scrub(Begin) → Scrub(Move, v) → Scrub(Commit)` through the REAL dispatch path — fast setup, no bypass, undo-correct by construction. Ships with P-I (it is three lines once the wire exists) + one flow using it as its own acceptance test.

**D9 — Widget catalog rides the surface layer (WIDGET_TREE §10 item 7, designed in).** The catalog is the declarations: a `--catalog` dump mode enumerating, per panel, every `ParamSurface` row id + `RowRole` + named chrome (the dump already serializes durable ids; this adds the enumeration view). No new protocol (widget-tree §5's rule). Ships with P-S. Kills the BUG-239 class structurally: a row without a queryable name cannot exist on the sanctioned path.

**D10 — Remaining WIDGET_TREE §10 items, dispositioned:** macros/settings sliders → P-S flows them through `param_surface` hosts (they are small; the point is zero unsanctioned row paths remain). `param_slots_to_ui` scratch buffer → P-P does it while touching `push_state` (pre-allocated scratch, INV-W4's no-new-per-frame-alloc rule makes it free to prove). With D4/D8/D9 this closes §10 entirely — supersession sweep at the final landing flips WIDGET_TREE's Deferred section to a tombstone pointing here.

**D11 — Regrowth guard is an invariant test.** `godfile_regrowth.rs` (manifold-app tests or repo-level xtask test): per-file line ceilings for every register-listed file, ceilings = post-split size + slack (exact numbers set at each phase landing), failing `cargo nextest` with a message pointing at the register. Precedent: `no_bespoke_row_infra` (INV-8). Hook-based guard rejected (review LOW-9: hooks can't see commit-level growth).

**Consequences, stated honestly:** (1) This wave touches the hottest merge surface in the repo while other work continues — strangler discipline (small landings, no mega-branch) mitigates, does not eliminate; conflicts land on the wave, not on feature work, by keeping each landing small. (2) `git blame` granularity across moved code degrades (mitigant: `--color-moved` review + `git log --follow`; a `.git-blame-ignore-revs` entry per pure-move landing is a deliverable of each landing). (3) The intent decomposition (P-D) churns every `PanelAction::X` emit site's *name* (`PanelAction::Inspector(InspectorAction::X)` or a `From` impl hides it) — mechanical but wide; the compiler is the checklist. (4) Until P-I lands, `DispatchCtx.scrub` carries the old ten `Option`s as one struct — an explicitly interim shape, named here, deleted by P-I in the same wave (not a deferred cleanup).

---

## 3. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| INV-G1 Pure-move commits have zero non-scaffold residue; scaffold (dispatch-split structural lines) is separately counted, pattern-bounded, capped ≤25/commit | `scripts/move_identity_check.py` — routing preservation proven by variant-census equality, not by reading arms; run per lane pre-gate and per landing batch |
| INV-G2 No register-listed file regrows | `godfile_regrowth` invariant test (D11), rides nextest |
| INV-G3 Scrub behavior parity | `undo_baseline` + `mapping_undo_baseline` suites green UNMODIFIED through P-I; scrub flows green |
| INV-G4 No structural work moves to per-frame path | `MANIFOLD_RENDER_TRACE=1` spot-check at every P-P/P-F landing (>20ms frame fails); review line "no new per-frame work/alloc" |
| INV-G5 Layer reachability: projection modules never send commands; dispatch modules never build tree nodes | negative `rg` gates per phase (`rg 'content_tx' projection/` → 0; `rg 'add_node|UITree' dispatch/` → 0) + `pub(crate)` visibility |
| INV-G6 One wire type at the boundary | `dispatch(action: &PanelAction, ctx)` remains the sole entry (`rg 'fn dispatch\(' ui_bridge` → 1) |
| INV-G7 Old symbols deleted, never paralleled | per-phase deletion gates (`rg` → 0 for: the 18-arg signature, dead trio variants, snapshot fields, `dispatch_inspector` as a single fn) |
| Widget-tree INV-1..8 | unchanged, keep passing throughout |

---

## 4. Phasing

Two workstreams; parallelism only where files don't overlap. Order within WS1 per review HIGH-3: **projection → bridge → intents(+scrub) → surface**. Every phase = one session, lands committable behind: focused nextest + `-p` clippy (lane pre-gate) → full sweep + flow suite + fixture round-trip + move-identity + INV gates (landing batch, main checkout). Pure-move and semantic commits never share a landing. Each phase brief below is the seed; the executing seat gets the full §5/§6-compliant brief generated from it at briefing time, with re-derivation commands (conformance treatment — later phases WILL find drifted line numbers; counts differ → stop and re-list).

**WS1 (ui_bridge):**
- **P-P Projection** *(pure moves + one named semantic rider)*: `state_sync.rs` → `projection/{transport,timeline,inspector,cards,scene,audio}.rs` along its existing function boundaries; `push_state` stays the per-frame entry, structural builders grouped per domain. Rider (own commit): `param_slots_to_ui` scratch buffer (D10). Gate adds INV-G4 trace run.
- **P-B Bridge split + context struct**: `DispatchCtx` (D3) FIRST as its own semantic commit — signature change on the existing giant fn, bodies untouched, compiler-driven; `SliderReset` recursion collapses to `dispatch(snapshot, ctx)`; the ctx shrinks every subsequent sub-dispatcher signature to `(action, ctx)`. THEN `dispatch_inspector` → `dispatch/` modules (D6, pure moves; entry deliverable = the 150-variant census) with `dispatch_inspector` retained as the ordered first-non-unhandled chain router. Gate: `undo_baseline` family green unmodified; INV-G6/G7.
- **P-D Intent decomposition** *(semantic, mechanical re-parenting)*: D5's sum enum; variant bodies verbatim; `From` impls; compiler-driven sweep of emit sites. Gate: full flow suite (the wire is exercised everywhere); zero behavior assertions change.
- **P-I Scrub wire** *(the deep semantic phase — Opus seat implements directly)*: D4 + D8. Enumerate `ValueRef` from the trio census; port one family first (slider drag — the `undo_baseline` fixture family) as the vertical slice, then families in mechanical batches; delete trios, snapshot fields, `ActiveInspectorDrag` families, editor drag structs. Gate: INV-G3 (the whole point), deletion gates, the D8 flow verb + its flow.

**WS2 (frame — parallel after P-P lands, no shared files with P-B/P-D):**
- **P-F1 Frame stages**: `tick_and_render` → `frame/` (D7, pure moves); `editor_bridge.rs` extraction; WGSL out of `app.rs`. Gate: INV-G4 trace run, flows, PNG look-oracle (unchanged layout).
- **P-F2 App slimming**: `Application` field-group regroups (mechanical); `ui_root.rs` split (panel wiring / overlay+drag / dropdown builders). Gate: same.

**Convergence:**
- **P-S Surface migration** *(deepest; Opus-direct; AFTER P-I so rows route through the final wire)*: `param_card.rs` — migration-vs-split census first (how much is bespoke infra that dies into `param_surface` machinery vs. genuine renderer/state code that relocates); macros/settings sliders onto hosts (D10); D9 catalog; `panels/inspector.rs`/`param_slider_shared.rs`/`scene_setup_panel.rs` along the same lines. Multiple sessions, phased by layer within the file set — the census decides the sub-phases, reviewed by Fable before briefing.
- **P-Z Supersession + ceilings**: WIDGET_TREE §10 tombstone; register updated (inventory, not status); regrowth ceilings finalized (D11); docs path-reference sweep (`CORE_ENGINE_MAP`, `WIDGET_TREE_DESIGN`, memory files); `gen_docs_index.py`; `.git-blame-ignore-revs` entries.

Phasing-completeness check: every §2 commitment lands in exactly one phase (D1/D2→P-P/P-B/P-F; D3→P-B; D4→P-I; D5→P-D; D6→P-B; D7→P-F1; D8→P-I; D9/D10→P-S (+P-P rider); D11→P-Z ceilings, test scaffold at first landing). Nothing deferred; nothing trigger-clause'd.

**Forbidden moves, wave-wide:** a "temporary" second dispatch entry point · keeping any old path alive behind a flag · reshaping variant bodies during P-D · a data-binding/subscription system anywhere · new `Arc<Mutex>` · adapters around a misfit call site (escalate: the seam has a gap) · improving adjacent code mid-move (notes file for Peter instead) · landing with a red gate.

## 5. Decided — do not reopen

1. Matrix: layers primary, domains secondary; vocabulary per D2, fixed campaign-wide.
2. Module seams + `pub(crate)` now; crate promotion is per-seam, later, evidence-based. (D1)
3. Scrub slots are gesture state, never projections; one owner per dispatch arg. (D3)
4. One wire type (`PanelAction` sum enum); per-domain closed intent enums beneath it. (D5)
5. Bridge splits before the enum decomposes; both before the scrub wire. (D6, review HIGH-3)
6. `ValueRef` reuses existing addressing — no new id scheme, ever. (D4; widget-tree D2)
7. Flow verbs go through the real wire; the catalog is the declarations. (D8/D9)
8. Pure-move proof = `move_identity_check` zero residue; `cargo public-api` rejected. (INV-G1)
9. Regrowth guard is a nextest invariant, not a hook. (D11)
10. WIDGET_TREE §10 closes with this wave; no revival triggers anywhere in this doc (Peter's directive).

## 6. Deferred

None. (Peter's directive: no deferred-with-revival-triggers. Wave 2/3 scope lives in the register, not here.)
