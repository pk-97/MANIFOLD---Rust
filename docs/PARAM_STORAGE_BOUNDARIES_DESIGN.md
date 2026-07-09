# Param Storage Boundaries — load reconcile, card single-source, migration self-containment

**Status:** IN PROGRESS · P1 SHIPPED (`wave/param-boundaries-p1`) · P3 SHIPPED (`wave/param-boundaries-p3`) · P2 not built · 2026-07-06 · Fable
**Prerequisites:** PARAM_STORAGE_DESIGN.md P1–P5 (all SHIPPED 2026-07-05) + the BUG-036
fix wave (`b2f78725`, `0434da5e`, 2026-07-06). Nothing else.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.
**Executor sizing:** written for Sonnet at medium effort — every phase is mechanical
transcription plus compiler-driven migration; no architectural choices remain open.

The param-storage redesign got the **model** right: one id-keyed `ParamManifest` per
instance, descriptor + state in one struct, order = card order. The 2026-07-06
post-refactor audit confirmed the model and found that every remaining defect lives at
the **boundaries** — where the manifest meets serde, the UI, and the one-time
migration. This design closes those three boundaries. Peter, 2026-07-06: *"please
write a new design and architecture document for Sonnet (medium effort) to implement
all of these upgrades"* — the upgrades being exactly the three tensions named in that
session's audit report.

The governing insight: **BUG-036 was not a missing `install` call; it was
deserialization depending on ambient global state.** The shipped fix (pre-pass hook +
keep-don't-drop backstop + overlay rollback) makes the ordering *correct by
discipline*. This design makes it *irrelevant by construction* — after P1, no load
path can be built that has the bug, and three pieces of defensive machinery shipped on
2026-07-06 get deleted because the hazard they guard no longer exists.

What this means on stage: nothing new — and that's the point. Loading a show, restoring
a snapshot, and the inspector's calibrated ranges already behave correctly today. This
wave removes the *ways they can silently regress*: a future load path added without the
magic ordering, a future card feature reading the stale copy of a spec. It is
insurance for the August authoring push, not a feature.

Companions: [PARAM_STORAGE_DESIGN.md](PARAM_STORAGE_DESIGN.md) (the model this
hardens; its §7 decided-items still bind), [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md)
(execution contract), `docs/BUG_BACKLOG.md` BUG-040 (P3's subject).

---

## 1. Audit — what exists (verified 2026-07-06, this session)

| Piece | Where | State |
|---|---|---|
| Manifest build at load | `manifold-core/src/effects.rs` (`build_param_manifest`, called from `PresetInstance`'s custom `Deserialize`) | Consults the global `preset_definition_registry` DURING serde. Informed-drop rule + `placeholder_spec` keep-don't-drop backstop landed `b2f78725`. |
| Descriptor seeding | `effects.rs` (`gather_known_params`) | Generator with inline graph → its `meta.params` is the descriptor authority; otherwise registry `param_defs`. |
| Pre-deserialize hook | `manifold-io/src/loader.rs` (`EmbeddedPresetsPrePass`, `load_project_with`, `load_project_snapshot_with`, `load_project_from_json_with`) | JSON pre-scan hands `embeddedPresets` to an installer before typed deserialize. **Deleted by P1** (superseded, not kept in parallel). |
| Overlay install | `manifold-app/src/project_io.rs` (`install_embedded_presets`, `install_project_preset_overlay`); unconditional install at the apply seam in `app_lifecycle.rs` (`apply_project_io_action`) | Keep. The apply-seam install (covers New Project + snapshot restore leaks) is permanent. |
| Overlay rollback | `manifold-renderer/src/preset_loader.rs` (`ProjectPresetsSnapshot`, `project_presets_snapshot`, `restore_project_presets`) + 3 call sites + test in `tests/project_preset_overlay.rs` | Guards "hook mutated overlay, then load failed". **Deleted by P1** — after P1 the install happens only after a successful deserialize, so the hazard window is gone. |
| Card spec rows | `manifold-app/src/ui_bridge/state_sync.rs:1888–2013` | Rows sourced from registry `param_defs` (effects) / graph `meta.params` (generators), then a per-instance graph-override `meta.params` overlay patches min/max/name/`is_angle`. Two sources, one dual-write. |
| The dual-write | `manifold-editing/src/commands/effects.rs:923–983` (`EditParamMappingCommand` region) | Calibration writes BOTH the manifest spec (the authority) AND the graph `meta.params` shadow (for the card overlay). **Deleted by P2.** |
| D12 derive-on-save | PARAM_STORAGE_DESIGN §2 D12 says `meta.params` is "derived on save from the manifest" | **Not implemented** — verified by grep 2026-07-06: no save-time rewrite exists; the shadow is maintained only by the dual-writes above. P2 implements D12 as written. |
| V1.4 migration | `manifold-io/src/migrations/param_storage_v14.rs` | Positional→id order comes from the instance's own graph, else the baked `LEGACY_PARAM_ORDER` table. Never consults the file's own `embeddedPresets` → BUG-040. |
| Regression tests | `manifold-app/tests/project_local_preset_reload.rs` (both load defenses), `manifold-renderer/tests/project_preset_overlay.rs` (overlay tiers + rollback), `effects.rs` unit `effect_instance_deserialize_params_without_registry_keeps_state` | All green on main. P1 modifies the first and third; the rollback test is deleted with its subject. |
| Instance walker precedent | `manifold-core/src/project.rs` (`tracking_preset_ids`) | Read-only walk over every `PresetInstance` home: master effects, layer effects, clip effects, `gen_params`. P1's mutable walker copies its coverage exactly. |

*Extend, don't redesign.* Everything below is wiring against these pieces; the only
genuinely new code is one loader stage (~40 lines) and one serialize wrapper (~30 lines).

## 2. Decisions

**D1 — Param reconciliation becomes an explicit loader stage; serde keeps today's
behavior as the fallback.** `PresetInstance::deserialize` continues to build the
manifest exactly as today (registry consult, informed-drop, placeholder backstop) *and
additionally stashes the raw wire map* in a transient field. The loader then, AFTER
overlay install, calls `Project::reconcile_param_manifests()`, which re-runs
`build_param_manifest` from each stash against the now-complete registry and replaces
the manifest. Ordering lives inside one loader function; no caller can get it wrong.
Rationale: a raw-deserialize-only variant (manifest left empty until reconcile) was
priced and **rejected** — dozens of existing unit tests and any direct
`serde_json::from_str::<PresetInstance>` user would silently see empty manifests;
double-building is a load-time-only linear pass over ≤ ~40 entries per instance
(microseconds against the canonical 53-layer fixture) and keeps serde self-contained.
Consequences, stated honestly: manifests are built twice per project load, and the
transient stash makes `PresetInstance` ~one pointer wider until reconcile clears it.

**D2 — The 2026-07-06 defensive machinery is deleted, not kept in parallel.** With
D1, the overlay is installed from the *typed, successfully-deserialized* project —
there is no window where a failed load has already mutated it, and no JSON pre-scan is
needed. `EmbeddedPresetsPrePass`, the pre-scan inside the `_with` fns, and the whole
`ProjectPresetsSnapshot` rollback API (+ its test) go. Rejected: keeping rollback "for
safety" — a guard for an unreachable state is exactly the silent-parallel-path pattern
the standard forbids; if a future change re-opens the window, the P1 round-trip gate
is what catches it, not dead code.

**D3 — The `_with` hook survives, retyped.** Public loader API stays
`load_project_with(path, installer)` / `load_project_snapshot_with(..)` /
`load_project_from_json_with(..)`, but the installer signature is unchanged —
`impl FnOnce(&[EmbeddedPreset])` — and is now invoked with `&project.embedded_presets`
*after* deserialize and *before* reconcile. App callers don't change at all
(`install_embedded_presets` already has that shape). Rejected: loader returning a
`RawProject` type-state the caller must finish — stronger in theory, but it pushes the
ordering obligation back out to every caller, which is the disease.

**D4 — Cards read the manifest; the shadow becomes derived-on-save only (D12 as
written).** `state_sync`'s spec rows are built from `inst.params` for BOTH kinds — the
manifest already carries everything a row needs (spec: min/max/name/`is_angle`/
`is_toggle`/`is_trigger`/`value_labels`/`whole_numbers`; state: `exposed`). The
registry-sourcing arm, the `user_param_bindings` append loop, and the graph-override
overlay block (state_sync.rs:1982–2013) are all deleted. `meta.params` stays on the
wire (generators with inline graphs seed from it at load; save-as-preset needs it) but
is **derived from the manifest at serialize time** via a wrapper, shaped like
`ManifestSer` in `effects.rs`; the `EditParamMappingCommand` dual-writes are deleted.
One fact, one owner, one derivation point. Consequences, stated honestly: any
*other* reader of the shadow specs (graph-editor popover, save-as-preset path) must be
inventoried at execution time (P2 read-back command below) — a reader that needs
fresher-than-last-save specs is an escalation, not an adapter.

**D5 — The V1.4 migration consults the file's own `embeddedPresets` before the baked
table (BUG-040).** For a positional generator instance without an inline graph, the
order source becomes: own graph `meta.params` → `embeddedPresets[type].def
.presetMetadata.params` → baked `LEGACY_PARAM_ORDER` → loud-drop. Pure `Value → Value`,
self-contained in the same JSON tree, quarantine intact. Rejected: regenerating the
baked table to include import-era types — the table is frozen by design ("never
regenerated", PARAM_STORAGE_DESIGN §4); the file itself is the right authority.

**Not in scope (already Deferred in PARAM_STORAGE_DESIGN §8 — do not revive here):**
registry eradication beyond containment, id interning, id-keyed transport,
`PresetDef` removal.

## 3. Committed shapes

New field + method (manifold-core):

```rust
// effects.rs — inside PresetInstance
/// Raw V1.4 wire entries, held between deserialize and the loader's
/// reconcile pass (D1). Never serialized; None everywhere else.
#[serde(skip)]
pub(crate) pending_wire: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
```

```rust
// project.rs
/// Rebuild every instance's ParamManifest from its stashed wire entries,
/// against the CURRENT registry (call after the project's embedded presets
/// are installed). Idempotent: instances with no stash are untouched.
/// Walks exactly the homes tracking_preset_ids walks (its mut sibling).
pub fn reconcile_param_manifests(&mut self) { .. }
```

`ParamEntryWire` becomes `pub(crate)` within manifold-core if it isn't already
reachable from `project.rs` (it stays crate-private; nothing outside core touches it).

Loader end-state (manifold-io, `loader.rs`) — the ordering, in one place:

```rust
pub fn load_project_from_json_with(
    json: &str,
    register_embedded_presets: impl FnOnce(&[EmbeddedPreset]),
) -> Result<Project, LoadError> {
    let migrated = migrate::migrate_if_needed(json)?;
    let mut project: Project = serde_json::from_str(&migrated)?;   // stashes wire
    register_embedded_presets(&project.embedded_presets);          // typed, post-parse
    project.reconcile_param_manifests();                           // registry complete
    project.strip_unknown_effects();
    project.on_after_deserialize();
    project.sync_bpm_from_tempo_map();
    Ok(project)
}
```

Serialize-time derivation (manifold-core, shaped like `ManifestSer`):

```rust
/// Serializes the instance's graph override with preset_metadata.params
/// rewritten from the live manifest (D12: derived on save, one owner).
struct GraphWithDerivedParams<'a> { graph: &'a EffectGraphDef, manifest: &'a ParamManifest }
```

⚠ VERIFY-AT-IMPL: the exact emission point of the `graph` field inside
`PresetInstance`'s custom `Serialize` (both the effect arm and
`serialize_as_generator`) — `rg -n "graph" crates/manifold-core/src/effects.rs`
within the two serialize fns, before writing the wrapper.

## 4. Phasing

### P1 — reconcile stage + delete the ordering machinery (one session) — SHIPPED

Landed on `wave/param-boundaries-p1` (worktree `.claude/worktrees/param-bound-p1`):
`pending_wire` stash + `Project::reconcile_param_manifests()` (with a
keep-vs-clear refinement not spelled out in §3 — see below); the loader
reshaped per §3, `EmbeddedPresetsPrePass` and the JSON pre-scan deleted; the
rollback API (`ProjectPresetsSnapshot` + both fns + 3 call sites + its test)
deleted; the three-arm `project_local_preset_reload` test green (order
independence proven live); round-trip gate green against
`~/Downloads/meshImportTests.manifold` (a real BUG-036 specimen — its saved
`gen_params.params` serializes empty; post-P1 all 17 template params resolve
and the `cam_orbit` driver resolves against a real one, zero
dropping/placeholder log lines).

Implementation note beyond §3's literal snippet: `pending_wire` does NOT
clear unconditionally on the first `reconcile_manifest()` call. If that pass
still can't resolve a template (`template_known_for` false — the
keep-don't-drop path), the stash is kept so a *later* reconcile call — after
the registry catches up, e.g. an overlay installed after the load already
returned — can retry from the same wire entries. It clears once a pass
resolves a real template (the common case: one load, one reconcile). This is
what makes the P1 brief's third test arm (bare load → install → reconcile
directly, order-independence) actually possible; a destructive `.take()` on
every call would make that sequence a no-op. Also: `#[serde(skip)]` from
§3's snippet doesn't apply here — `PresetInstance` has hand-written
`Serialize`/`Deserialize` impls (not derived), so the attribute isn't a
registered helper and won't compile; the field is simply never written by
the custom `Serialize` impl, achieving the same "never on the wire" property
without the attribute.

Re-derivation audit finding: the doc's call-site count (3 loader fns, 3 app
sites, 2 tests) undercounted by one — `manifold-app/src/ui_snapshot/fixtures.rs:69`
(`project_scene`, the headless ui-snap harness) also calls `load_project_with`.
No behavior change was needed there (D3 keeps the installer signature and
contract unchanged), so only its stale comment (referencing the deleted
pre-deserialize hook) was corrected.

**Entry state:** `git log --oneline -1` includes or descends from `0434da5e`;
`cargo test -p manifold-app --test project_local_preset_reload` green;
`rg -n "EmbeddedPresetsPrePass" crates/manifold-io/src/loader.rs` returns the struct.

**Read-back first:** this doc §2 D1–D3 + §3; PARAM_STORAGE_DESIGN §4 (load
reconcile rules — they still govern `build_param_manifest`, unchanged);
`loader.rs` whole; `build_param_manifest` and its callers
(`rg -n "build_param_manifest" crates/manifold-core/src`). Restate the decisions,
the forbidden moves, and what the entry checks found before any edit.

**Deliverables:**
- `pending_wire` stash + `Project::reconcile_param_manifests()` (walker coverage
  copied from `tracking_preset_ids`; add a mut-walker helper next to it if none
  exists rather than open-coding four loops twice).
- Loader fns reshaped per §3; `EmbeddedPresetsPrePass` and the JSON pre-scan deleted.
- Rollback API deleted: `ProjectPresetsSnapshot` + both fns in `preset_loader.rs`,
  the three call sites (`project_io.rs`, `app_lifecycle.rs` ×2), and the
  `overlay_snapshot_restores_after_a_candidate_install` test.
- `project_local_preset_reload.rs` updated: the hook arm now passes the typed
  installer (same signature — minimal churn); ADD a third arm proving order
  independence by construction: deserialize via bare `load_project_from_json`
  (no installer), then install the overlay, then call
  `reconcile_param_manifests()` directly and assert full template resolution —
  the sequence that was impossible pre-P1.

**Seam brief:** old → new is §3 verbatim; the public fn names and the installer
signature do not change, so app call sites are untouched except deleting the three
rollback lines. Compiler-driven: delete `EmbeddedPresetsPrePass` and
`project_presets_snapshot` FIRST; the red build is the checklist.
Re-derivation: `rg -n "load_project_with|load_project_snapshot_with|load_project_from_json_with|project_presets_snapshot" crates --type rust`
— if call sites exceed the audit's list (3 loader fns, 3 app sites, 2 tests), stop
and list before touching.

**Forbidden moves:** keeping the JSON pre-scan or rollback "just in case" (parallel
old path); a `reconciled: bool` checked at call sites (flag-as-deferral — reconcile
is unconditional inside the loader); lazy reconcile-on-first-access (re-introduces
hidden global ordering); widening into P2's card work.

**Gate.** Positive: `cargo test -p manifold-core -p manifold-io -p manifold-app`
green; the three-arm reload test green. Round-trip gate (§5 of the standard): load
`~/Downloads/meshImportTests.manifold` headlessly via the scratch pattern from the
BUG-036 session (throwaway test, deleted after) — all 17 params present, `cam_orbit`
driver resolves, zero "dropping unknown param id" lines. Negative:
`rg -n "EmbeddedPresetsPrePass|project_presets_snapshot|restore_project_presets" crates` → **zero hits**.
Test scope: focused crates above + `cargo clippy --workspace -- -D warnings`; no
workspace test sweep (P3 runs the single sweep for the wave).
**Demo:** none visible — L1, plus the L2 repro-load log artifact above.

### P2 — card single-source + derive-on-save; the dual-write dies (one session)

**Entry state:** P1 landed (`rg "reconcile_param_manifests" crates/manifold-io` hits);
`cargo test -p manifold-app --test user_param_bindings_e2e` green.

**Read-back first:** this doc D4; state_sync.rs:1880–2060 (the whole card-config fn);
`EditParamMappingCommand` whole (`manifold-editing/src/commands/effects.rs:900–1000`
region); the shadow-reader inventory — run
`rg -n "preset_metadata" crates/manifold-app/src crates/manifold-renderer/src --type rust | rg "\.params"`
and classify every hit *display-read / save-path / load-seed* in the session notes
BEFORE editing. A hit that needs fresher-than-last-save specs and isn't the card
path is an escalation, not an adapter.

**Deliverables:**
- `state_sync` spec rows built by one iteration over `inst.params` (both kinds);
  registry arm, user-binding append loop, and overlay block (1982–2013) deleted.
- `GraphWithDerivedParams` serialize wrapper wired into both serialize arms
  (⚠ VERIFY-AT-IMPL marker in §3 resolves here).
- Dual-writes to `meta.params` in `EditParamMappingCommand` (and the expose command
  if the inventory finds it shadow-writing) deleted — the manifest spec write stays.
- Regression: extend `user_exposed_angle_param_carries_is_angle_through_manifest_and_synth`
  (or sibling) to assert a *calibrated* param's card row min/max comes from the
  manifest after a save/reload round trip with NO shadow write in the code path.

**Forbidden moves:** keeping the dual-write "for safety" (two owners of one fact —
the bug class this wave exists to delete); a registry fallback when a manifest is
unexpectedly empty (silent fallback — post-P1 an empty manifest on a card is a bug;
`log::error` and show nothing rather than mask it); reordering rows "while here"
(card order is manifest order, already correct).

**Gate.** Positive: focused tests for editing + app; the calibration round-trip test;
byte-comparison guard that a saved project/preset containing a calibrated +
user-added param round-trips with `meta.params` matching the manifest (write the
fixture in-test, assert JSON equality of the derived section). Round-trip gate:
calibrate → save → reload → card row shows calibrated range. Negative:
`rg -n "meta\.params" crates/manifold-editing/src/commands/effects.rs` → zero
*write* sites (reads may remain if the inventory blessed them);
the overlay block gone from state_sync (`rg -n "single reshape source" crates/manifold-app/src/ui_bridge/state_sync.rs` → zero).
**Demo (L3):** a `scripts/ui-flows/` flow (copy `select-and-inspect.json`) that opens
the inspector on a calibrated param and asserts the displayed range text — the
performer gesture: *drag a calibrated Camera Orbit slider and see its real degree
range, after a reload*.

### P3 — migration reads `embeddedPresets` (BUG-040) + wave sweep (one short session) — SHIPPED

Landed on `wave/param-boundaries-p3`. The D5 lookup arm ("case 1.5") sits between
the existing own-graph arm and the WireframeDepth/baked-table arms in
`positional_ids` (`param_storage_v14.rs`): a generator instance with no own
`graph.presetMetadata.params` now consults a lookup built once per `migrate()`
call from the file's own `embeddedPresets` (`embedded_param_orders`, read-only,
built before the mutable per-instance walk) before falling through to the frozen
baked table. Three new tests: matching-embedded-preset resolves by its order,
no-match falls through to the baked table/loud-drop (BUG-040's two required
fixtures), and a third proving the own-graph arm still wins when both exist.
BUG-040 entry updated to FIXED in the same commit.

Full workspace sweep run: 6 pre-existing failures found in
`manifold-renderer::ui_cache_manager` (D4 region-ownership panic, `tree.rs:290`)
— confirmed via `git merge-base --is-ancestor` that the causing commit
(`0bb51dad`) is already on `main`, predating this entire wave; logged as BUG-077,
not fixed here (out of scope — a `manifold-ui`/`manifold-renderer` UI-region test
gap, unrelated to param storage). All `manifold-io` tests, including the BUG-040
fixtures, are green; clippy is clean.

**Entry state:** P1 landed (P3 is independent of P2; run in either order after P1).
**Read-back:** BUG-040 entry in `docs/BUG_BACKLOG.md`; `param_storage_v14.rs` §
around the order-source resolution (`rg -n "LEGACY_PARAM_ORDER" crates/manifold-io/src/migrations/param_storage_v14.rs`).
**Deliverables:** the D5 lookup arm; two fixtures — positional generator instance
WITH matching embedded preset (values land by that order) and WITHOUT (falls to
baked table / loud-drop unchanged). Update the BUG-040 entry to FIXED in the same
commit. **Forbidden moves:** consulting the live registry from the migration
(quarantine rule — the module reads only the JSON tree and its baked tables);
regenerating the baked table. **Gate.** Positive: migration unit tests incl. the
two fixtures; **this phase runs the wave's single full workspace sweep** +
clippy. Negative: `rg -n "preset_definition_registry" crates/manifold-io/src/migrations/` → zero hits.
**Demo:** none — L1 (pure data migration; fixtures are the artifact).

## 5. Decided — do not reopen

1. Reconcile is an explicit loader stage; serde keeps building manifests too
   (double-build is accepted; raw-only deserialize rejected for test-surface churn).
2. The pre-scan hook, `EmbeddedPresetsPrePass`, and the overlay rollback API are
   deleted in P1 — not kept alongside.
3. Loader public API names and installer signature are unchanged.
4. Cards read the manifest; `meta.params` is derived-on-save only; dual-writes die.
5. The migration's order authority chain: own graph → file's `embeddedPresets` →
   baked table → loud drop. The baked table is never regenerated.
6. Registry eradication, interning, id-keyed transport: still Deferred
   (PARAM_STORAGE_DESIGN §8 owns them).

## 6. Deferred

- **Raw-deserialize-only serde** (manifest built exactly once, in reconcile) —
  revive only if the double-build ever shows up in a load-time profile of the
  canonical fixture; it won't at current scale.
- **Deleting `keep-don't-drop`** — the backstop stays even after P1 (it guards
  serde-direct contexts and any future non-loader path). Revive deletion only if
  registry eradication lands and makes template resolution infallible.
