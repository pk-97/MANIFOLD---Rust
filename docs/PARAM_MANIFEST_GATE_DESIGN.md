# PARAM_MANIFEST_GATE — make a half-built param manifest unobservable at runtime (BUG-080)

**Status:** P1 SHIPPED 2026-07-14 (bug-wave lane B) · Sonnet 5 (Peter approved the direction same day: "I want to also ensure these bugs are fixed at the root and fundamental level … remove bug classes where possible and sensible") · `manifest_provisional()` (`crates/manifold-core/src/effects.rs`), the two seam asserts + throttled warns (`crates/manifold-renderer/src/preset_runtime.rs`'s `assert_manifest_gate`, `crates/manifold-app/src/ui_bridge/state_sync.rs`'s `rows_from_manifest`), the D3 meta-test (`crates/manifold-core/tests/bug080_project_deserialize_single_door.rs`), and the two INV-1 tests all landed; gate green (1721/1721, `-p manifold-core -p manifold-renderer -p manifold-io`).
**Prerequisites:** PARAM_STORAGE_BOUNDARIES_DESIGN.md P1 (SHIPPED — the reconcile stage this design hardens)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting the phase. Executes inside the 2026-07-14 bug-wave **lane B** session.

The governing insight: BUG-080's "partially built manifest is an observable state" does
not need a new construction pipeline — PARAM_STORAGE_BOUNDARIES D1 already built the
gate (deserialize builds + stashes, the loader reconciles). What's missing is that the
provisional state is *silent*: `pending_wire.is_some()` already IS the structural marker
for "this manifest was built against an incomplete registry," but nothing observes it at
the seams where a degraded manifest becomes user-visible. So a future load/ingest path
that forgets the reconcile call inherits empty/placeholder knobs silently (the BUG-036
class). The class fix is to make the existing marker loud at the runtime seams and to
pin the loader as the single production door with a machine check — detection at the
seam, not a 255-site type-state sweep.

On stage: this is the class of failure where a project opens and effect cards show
missing or placeholder sliders. The fix means that state can no longer be reached
silently — it either can't happen (loader path) or screams in dev and logs loudly in
release before Peter hits it mid-set.

## 1. Audit — what exists (verified 2026-07-14)

| Piece | Where | State |
|---|---|---|
| Deserialize-time build + wire stash | `crates/manifold-core/src/effects.rs:1344` (custom `Deserialize`), stash field `pending_wire` at `effects.rs:618` (private) | Ships (BOUNDARIES D1). Manifest built against whatever registry is visible; wire stashed for retry |
| Retryable per-instance reconcile | `PresetInstance::reconcile_manifest`, `effects.rs:1599` | Ships. Keeps the stash while the template is unknown; clears it only on a real resolve — already idempotent and retry-safe |
| Project-wide pass | `Project::reconcile_param_manifests`, `crates/manifold-core/src/project.rs:1143`, walking `for_each_preset_instance_mut` (`project.rs:1108`) | Ships. Returns unresolved count into `load_report` (BUG-079 toast) |
| The one production gate | `crates/manifold-io/src/loader.rs:224` (from_str → register embedded → reconcile → strip → on_after_deserialize) | Ships. **Convention, not structure** — nothing stops a second door |
| Production deserialize entries outside the loader | rg sweep 2026-07-14 for `from_str::<Project>` / `from_value::<Project>` / `from_str::<PresetInstance>` etc. | **Zero hits outside tests** — the hazard is future paths, not current ones |
| Runtime seam: chain build | `PresetRuntime::try_build`, `crates/manifold-renderer/src/preset_runtime.rs:811` (takes `&[PresetInstance]`) | Where a degraded manifest becomes passthrough/wrong-params |
| Runtime seam: UI rows | `crates/manifold-app/src/ui_bridge/state_sync.rs:1971` (manifest → spec rows fn; call sites 811/827/842 via `param_slots_to_ui`) | Where a degraded manifest becomes missing/placeholder knobs |
| Direct `.params` reads workspace-wide | rg count 2026-07-14 | **255** — this number prices (and kills) any accessor/type-state migration |

Extend, don't redesign: every deliverable below attaches to a piece in this table.

## 2. Decisions

**D1 — Expose the provisional state; do not re-model it.** Add
`pub fn manifest_provisional(&self) -> bool { self.pending_wire.is_some() }` to
`PresetInstance` (next to `template_unresolved`, `effects.rs:1621`). `pending_wire`
already is the type-state; a parallel flag or enum would be a second copy of the same
fact. **Rejected:** wrapping `ParamManifest` in a `Provisional/Reconciled` type-state
enum — 255 direct `.params` read sites make that a workspace-wide sweep that adds no
information the boolean doesn't carry (`dont-cascade-redesign`: inventory decides blast
radius).

**D2 — The two runtime seams assert the invariant; release builds log once.** At the
top of `PresetRuntime::try_build`'s per-instance loop, and at the manifest-row
translation fn (`state_sync.rs:1971`):
`debug_assert!(!inst.manifest_provisional(), "BUG-080: provisional manifest reached runtime — a load/ingest path skipped reconcile_param_manifests()")`,
plus a release-mode once-per-instance throttled `log::warn!` (shape it like the BUG-038
once-then-quiet throttle in the Ableton bridge). Rationale: these are the exactly-two
places a half-built manifest becomes user-visible, and every production frame passes
them — a skipped gate cannot stay silent. **Rejected:** asserting inside a `params()`
accessor (requires privatizing the field — the same 255-site sweep as D1's rejection,
for the same detection coverage).

**D3 — The loader stays the single production door, machine-checked.** A workspace
meta-test (shape it like the docs-index freshness test) that rg's the workspace for
`from_str::<Project>`, `from_value::<Project>`, `from_reader::<Project>` and the
`PresetInstance` equivalents, and fails on any hit outside `crates/manifold-io/`,
`#[cfg(test)]`/`tests/` code, and `ui_snapshot` fixtures. **Rejected:** sealing the
`Deserialize` impl — serde traits are public API the loader and tests legitimately use;
the check is the enforceable form.

**D4 — Future ingest/paste/merge paths inherit the rule for free.** Any new path that
deserializes instances must end in `reconcile_param_manifests()` (idempotent and
retry-safe per the audit, so calling it "again" is always harmless). D2's detector is
what catches forgetting; this doc records the rule so the detector firing has a named
fix.

## 3. Invariants & enforcement

| Invariant | Enforcement (machine check) |
|---|---|
| INV-1: no provisional manifest is observable at runtime | The two D2 asserts. Proven by named tests: `bug080_provisional_manifest_asserts_at_chain_build` (debug test: bare `from_str` a project fixture whose instance tracks a project-embedded preset, skip reconcile, drive the seam, catch the panic) and `bug080_loader_path_never_provisional` (same fixture through `load_project_from_json_with` → zero provisional instances) |
| INV-2: Project deserialize has one production door | The D3 meta-test, `bug080_project_deserialize_single_door` |
| INV-3: reconcile is idempotent/retry-safe | Already enforced — the `project_local_preset_reload` three-arm test (BOUNDARIES P1); name it in the read-back, don't re-prove it |

## 4. Phasing — P1 only (one session, runs as part of bug-wave lane B)

**Entry state:** anchors re-verified: `rg -n 'pending_wire' crates/manifold-core/src/effects.rs` (fields at ~618, reconcile at ~1599), `rg -n 'reconcile_param_manifests' crates/manifold-io/src/loader.rs` (one call), `rg -n 'fn try_build' crates/manifold-renderer/src/preset_runtime.rs`. Counts that differ from the audit table → stop, list, proceed against the fresh inventory.

**Read-back (mandatory first step):** this doc whole; PARAM_STORAGE_BOUNDARIES_DESIGN.md §2 D1–D3; `effects.rs:1590–1625`. Restate: the two seams, the three rejections, the forbidden moves.

**Deliverables:** `manifest_provisional()` (D1) · the two seam asserts + throttled release warns (D2) · meta-test `bug080_project_deserialize_single_door` (D3) · tests `bug080_provisional_manifest_asserts_at_chain_build` + `bug080_loader_path_never_provisional` (INV-1) · this doc's Status flipped at landing.

**Gate:** `cargo nextest run -p manifold-core -p manifold-renderer -p manifold-io --lib` green including the three named tests; negative: the meta-test itself is the rg gate. Full workspace sweep at landing time per the standard §5 test-scope rule.

**Demo:** none — L1. No user-visible surface; the artifact is the assert-fires/loader-clean test pair.

**Forbidden moves (this phase's temptations, by name):** clearing `pending_wire` anywhere except `reconcile_manifest`'s successful-resolve branch (that silently destroys the marker this design depends on) · adding a `manifests_reconciled: bool` on `Project` (second copy of a per-instance fact) · touching any of the 255 `.params` read sites · widening into the BUG-158/two-way-binding territory (separate bug, separate brief) · `unwrap()` on the seam paths.

## 5. Decided — do not reopen

1. Detection at the two runtime seams, not accessor privatization (255 sites priced it out).
2. `pending_wire.is_some()` is the provisional marker; no parallel flag.
3. Loader is the single production door; meta-test enforces.
4. Build-once-in-reconcile stays rejected (BOUNDARIES D1 priced it: tests + bare deserializers would see empty manifests).

## 6. Deferred

- **Accessor migration / field privatization** — revive if the D2 detector ever fires in production more than once (that would prove new doors keep appearing and convention+detector isn't holding).
- **One-shot build via `DeserializeSeed` (registry threaded through serde)** — revive only if the double-build shows up in a load profile of the canonical 53-layer fixture; today it's microseconds.
