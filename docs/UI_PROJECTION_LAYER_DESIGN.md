# UI ↔ Content Projection Layer — enforcing the snapshot seam (A1)

**Status:** ⚠ PRE-FABLE DRAFT — audit complete; Q-GROWTH answered (Peter, 2026-07-09: many new
screens coming). That fact is settled; the **recommendation** it points to is C-then-A (§2), for
Peter/Fable to ratify at the window — not locked here. The open call for Fable is how Shape A is
realized (§2.2). Not PROPOSED yet.
**Prerequisites:** UI_HARNESS_UNIFICATION (approved, Sonnet-executing) closes the *verification*
half — the real UICacheManager render path in the headless harness. This design is the
*construction* half. They compose; neither blocks the other's P0.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before any phase. Design method:
DESIGN_AUTHORING.md (this doc's audit was run against §2; its kill-test is FOUNDATIONAL_GAPS A1's).

## 0. Binding constraints (named first, per DESIGN_AUTHORING §1)

Two of the five bind, and they decide the shape more than the feature does:

- **Thread residency (conform, never renegotiate).** Content thread owns `Project`; the UI thread
  sees it only through `ContentState` snapshots on a bounded channel; commands go the other way.
  Settled house model — this design makes the *seam* enforceable, it does not move it.
- **Hot path (hard exemption).** `ContentState.modulation_snapshot` is rebuilt **every content
  frame** ([content_state.rs:290](../crates/manifold-app/src/content_state.rs#L290) `capture_into`)
  as a hand-tuned zero-alloc flat-buffer packer with D8 topology guards. Per CLAUDE.md hot-path
  discipline this must **not** be routed through any generic/declarative mechanism. Any projection
  layer that touches it is wrong by construction.

Persistence does **not** bind: `ContentState` is transient, never serialized. Time-model does not
bind. That keeps the blast radius off the two most expensive constraints.

## 1. Audit — what exists (verified 2026-07-09)

The seam is three concrete pieces, all in `manifold-app`:
- **Emit side:** [content_state.rs](../crates/manifold-app/src/content_state.rs) — the `ContentState`
  struct (~70 fields) built each content tick, plus the `ModulationSnapshot` packer.
- **Apply side:** [ui_bridge/state_sync.rs](../crates/manifold-app/src/ui_bridge/state_sync.rs)
  (**2457 lines, the highest-churn non-app file — ~79 fix-commits since March**) — `push_state`
  (:173), `sync_project_data` (:850), `sync_inspector_data` (:1234). Each field is hand-consumed.
- **Translation boundary:** [ui_translate.rs](../crates/manifold-app/src/ui_translate.rs) (673 lines).

FOUNDATIONAL_GAPS A1 says the mechanism is *well-built and mapped* (UI_ARCHITECTURE_AUDIT.md,
2026-06-18); what's missing is **enforcement**. A1 pre-registered a kill-test: *survey the actual
snapshot fields; kill the declarative layer if they're too irregular to tabulate — the tell is a
"declarative" layer that's mostly escape hatches.* This audit runs that survey.

### 1.1 Field survey — the kill-test data

Classifying every `ContentState` field by projection shape (this is the whole design decision):

| Class | ~count | Shape | Regular? |
|---|---|---|---|
| **Scalar mirror** | ~40 | engine getter → scalar field → UI reads/formats | **yes** — tabulatable |
| One-shot event | 2 | `Option<T>`, set on the tick it happens, fires a toast (`export_finished`, `undo_redo_event`) | own lifecycle |
| Structural snapshot | 1 | `project_snapshot: Option<Arc<Project>>`, only on `data_version` change | bespoke, correct |
| **Hot-path packer** | 1 | `modulation_snapshot` — per-frame zero-alloc flat buffer (§0) | **must stay bespoke** |
| Gated live overlay | ~16 | "empty unless X open", perf-sensitive: the `spectrogram_*` cluster (11), editor/graph (`active_graph_snapshot`, `node_preview_info`, `live_node_params`, `node_atlas_layout`, `clip_atlas_layout`) | irregular per-cluster |

**Verified rot (negative claim, checked 2026-07-09).** The struct's own FIXME
([content_state.rs:59](../crates/manifold-app/src/content_state.rs#L59)) names fields "written by
content thread but never read by UI." Confirmed by grep across `manifold-ui` + the UI-read side:
`link_tempo`, `link_is_playing`, `midi_clock_bpm`, `osc_receiving_timecode`, `osc_timecode_display`,
`led_initialized` — **6 fields, 0 read sites each.** Every verified orphan is in the scalar-mirror
class. This is the rot A1 predicts: a field's emit half lands, its consume half never does (or gets
deleted), and nothing catches the orphan. The whole struct carries `#[allow(dead_code)]` to paper
over it.

### 1.2 Audit finding that corrects A1's framing

A1's bug-evidence list (BUG-015, 026, 036, 060) is **mostly not snapshot-projection bugs** — read
end-to-end this session: BUG-015 and BUG-060 are `UICacheManager`/atlas stale-pixel bugs (A2
territory, already fixed/addressed), BUG-026 is a missing animation poll, BUG-036 is load-ordering
(embedded presets registered after param deserialize). **This design fixes hand-threading churn and
orphan-field rot; it does NOT fix stale pixels.** Claiming otherwise is the
`dont-overclaim-plumbing-as-visual` trap. The honest justification is the 79-commit churn + the 6
verified orphans, not the render bugs.

## 2. Decisions

The kill-test verdict is **scoped-yes, not blanket-yes.** A single declarative table over all ~70
fields would be mostly escape hatches (the ~18 event/snapshot/hot-path/overlay fields each need
special handling) — that version is the plausible-wrong turn (`dont-cascade-redesign`). A
declarative binding over the **scalar-mirror class** (~40 fields) is genuinely regular and holds
100% of the verified rot. So the enforcement is scoped to the mirror class; the other four classes
are **named, visible exemptions**, not silently absorbed.

Two genuinely different shapes were priced (boundary moved a layer, per DESIGN_AUTHORING §4):
**Shape A** — a declarative mirror table that generates the field + capture write, so an undeclared
field can't reach the UI; **Shape B** — a `Projected<T>` wrapper that tracks whether each field was
read and fails a test on any it wasn't. The kill-pass on Shape A (my first instinct, and the
executor's per §5) surfaced a third, cheaper option hiding underneath:

**Shape C — the orphan-coverage test alone.** A single enforcement test that every `ContentState`
field has an emit *and* a consume site kills the whole verified rot class with near-zero
architecture (`eliminate-bug-class-at-storage-layer`, minimal form). It does nothing for churn.

So the real fork was never A-vs-B — it was **"how much beyond Shape C,"** and that turned on one
product fact only Peter has: how many new snapshot-bearing screens the release adds.

> **Q-GROWTH — answered (Peter, 2026-07-09):** *"Many Many new screens pages and interactive new
> UI is coming soon."*

**Recommendation (rests on the settled fact above; for the window to ratify) — build C, then A.**
Shape C ships first regardless: cheap, fork-independent, kills the verified
rot. Shape A is now justified — with many new screens, the hand-written emit/apply pair gets paid
dozens more times, and **interactive** screens add the error-prone part specifically: a control the
user drags needs the engine's incoming snapshot to *not* overwrite the value mid-drag
(drag-suppression), and hand-wiring that per control is exactly how stale/fighting-knob bugs breed.
A owns that declaration, so its payoff rises with interactivity, not just field count.

*Consequences, stated honestly:* A adds an indirection layer and (per §2.2) likely a codegen step —
new fields go through a declaration instead of a struct edit. That is the trade being bought: a
little ceremony per field in exchange for the orphan class being impossible and the drag/dirty rules
being declared in one legible place instead of re-derived in `state_sync.rs` each time.

### 2.1 Shape A, concretely
One declaration per mirror field — the three things A1 named (source, drag behavior, dirty
condition):

```
mirror! {
    bpm:            f64  from |e| e.bpm(),                    dirty: on_change, drag: none,
    is_playing:     bool from |e| e.is_playing(),            dirty: on_change, drag: none,
    master_opacity: f32  from |p| p.settings.master_opacity, dirty: on_change,
                                                             drag: suppress_while(Drag::MasterOpacity),
}
```
It generates the `ContentState` field, the capture-side write, and the consume hook with the
drag-suppression guard baked in. A field not declared can't reach the UI (I3); a declared field with
no consumer fails the orphan test (I1). The ~18 bespoke fields live in a separate, explicit
`bespoke!` block the mirror machinery excludes — a **named** exemption, never a silent escape hatch.

### 2.2 The one call left for Fable — how A is realized
Three ways to build the same declaration, same guarantee-shape, different costs:
- **proc-macro** (compile-time): strongest form of I3 ("undeclared field doesn't compile"), but the
  generated code is opaque to a session debugging a wiring issue.
- **table + build.rs**: generates a plain, greppable `.rs`; costs a build step; guarantee still
  compile-time.
- **runtime registry**: no codegen at all; simplest; guarantee weakens from "doesn't compile" to
  "fails a test."

My lean: **build.rs-generated table.** With many sessions adding fields fast, greppable generated
code matters more than the macro's slightly stronger guarantee — paying the proc-macro's
debug-opacity cost is backwards when the whole point is making field-adding cheap and legible.
Honest cost: a build step, and generated code in the tree. This is the taste call to spend the
window on; my lean is priced but not decided — Fable kill-passes it.

## 3. Invariants & enforcement

- **I1 — no orphan fields.** Every `ContentState` field has an emit site and a consume site.
  *Enforcement:* the Shape-C coverage test (ships first in P0, fork-independent). Removes the
  `#[allow(dead_code)]`.
- **I2 — the hot-path packer is exempt and stays exempt.** `modulation_snapshot` is never routed
  through the projection mechanism. *Enforcement:* it lives in the explicit `bespoke!` block the
  mirror machinery excludes; a test asserts `modulation_snapshot`/`project_snapshot` are not
  table-driven.
- **I3 — a mirror field that skips the declaration can't reach the UI.** *Enforcement:* depends on
  §2.2's realization — compile-time for proc-macro/build.rs, a test for the runtime registry.

## 4. Phasing

- **P0 — orphan-coverage test + delete the 6 dead fields (Shape C).** Fork-independent, cheap,
  ships the verified win. Removes `#[allow(dead_code)]`. Gate: the coverage test is red before the
  deletions, green after; `rg` proves the 6 fields gone. *Vertical slice:* the whole path for one
  field-class — emit, consume, enforcement — proven once, thinly.
- **P1 — declarative mirror (Shape A), realization per §2.2.** Table over the ~40 mirror fields with
  source/drag/dirty declarations; explicit `bespoke!` exemption block for the other four classes;
  migrate the mirror fields off their hand-written `state_sync.rs` pairs. Gate: named tests that an
  undeclared field is rejected and a declared-but-unconsumed field fails; byte-identical UI render
  before/after the migration of a sample field. Real brief written once §2.2 resolves.

## §. Decided — do not reopen
1. Q-GROWTH is answered — many new screens are coming (Peter, 2026-07-09). The *fact* is settled;
   the C-then-A build plan it points to is a recommendation for the window to ratify, not locked.
2. The hot-path `modulation_snapshot` packer is exempt from any projection mechanism (§0, I2).
3. Enforcement is scoped to the scalar-mirror class; events/snapshot/overlays are named exemptions,
   not absorbed (kill-test verdict).
4. This design does not claim to fix the stale-pixel bugs (BUG-015/060 etc.) — those are cache/A2
   (§1.2).

## §. Deferred
- **How Shape A is realized (§2.2)** — proc-macro vs build.rs vs registry. Not deferred *whether*,
  only *how*; resolves at the Fable window, then P1's brief is written.
- Folding A1 into FOUNDATIONAL_GAPS as resolved — at landing, per the STRUCTURAL_AUDIT_VERDICTS
  convention (keep the branch diff to the design files).
