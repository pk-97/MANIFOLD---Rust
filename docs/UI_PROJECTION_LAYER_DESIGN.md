# UI ↔ Content Projection Layer — enforcing the snapshot seam (A1)

**Status:** ⚠ PRE-FABLE DRAFT — audit complete, architecture fork OPEN. Not PROPOSED yet.
Opus authored the audit + priced the alternatives; the D-choice in §2 is staged for Fable's
review window (small budget, ~2026-07-09 +3h) and Peter's one product input (§2, Q-GROWTH).
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

## 2. Decisions — architecture fork (OPEN, staged for Fable)

The kill-test verdict is **scoped-yes, not blanket-yes.** A single declarative table over all ~70
fields would be mostly escape hatches (the ~18 event/snapshot/hot-path/overlay fields each need
special handling) — that version is the plausible-wrong turn (`dont-cascade-redesign`). A
declarative binding over the **scalar-mirror class** (~40 fields) is genuinely regular and holds
100% of the verified rot. So the enforcement is scoped to the mirror class; the other four classes
are **named, visible exemptions**, not silently absorbed.

Within that scope, two genuinely different shapes (boundary moved a layer, per DESIGN_AUTHORING §4):

**Shape A — declarative mirror table / codegen.** One table declares each mirror field
`(source getter, drag-suppress, dirty condition)`; the `ContentState` field + capture-side write are
generated; a field not in the table can't reach the UI. Orphan = a table entry with no consumer,
caught by walking the table.
- *Cost:* macro/codegen machinery (real indirection). *Forecloses:* per-field bespoke **apply**
  logic — but the survey shows the apply side is legitimately varied (the MIDI/OSC/Link chips
  *format*, they don't just mirror), so the table cleanly owns emit + existence + orphan-detection,
  and the apply half stays hand-written regardless. Heavy machinery whose guarantee lands mostly on
  the emit half.

**Shape B — `Projected<T>` wrapper + consume-tracking.** Each mirror field becomes a wrapper that
records whether it was read across a frame; a headless-frame test (or debug-assert) fails on any
field emitted but never consumed. Boundary moves to the *read* side.
- *Cost:* wrapping the mirror fields; guarantee is a **test**, not a compile error. *Forecloses:*
  little — incremental, can wrap fields one at a time.

**Kill-pass on the favorite (Shape A).** My first instinct — and, per DESIGN_AUTHORING §5, the
executor's — is "build the declarative projection framework," because it sounds like the root fix.
The kill-test data refutes the strong form: the codegen's biggest promised win is organizing the
emit side, but the *verified* pain is (a) orphans and (b) churn — and orphans die to a far cheaper
thing:

**Shape C (fell out of the kill-pass) — the orphan-coverage test alone.** A single enforcement test
that every `ContentState` field has both an emit site and a consume site kills the entire verified
rot class with near-zero architecture (`eliminate-bug-class-at-storage-layer`, minimal form). It
does nothing for churn.

So the real fork is not A-vs-B but **"how much beyond Shape C."** Shape C is cheap, high-confidence,
and I'd ship it first regardless. The question of whether to build the churn-reducing declarative
mirror (A) on top of it is a **product/taste call that depends on future field growth** — which is
Peter's input, not the codebase's:

> **Q-GROWTH (for Peter, at the window):** how many new snapshot-bearing screens does the release
> work add? The release is authoring + export with new surfaces (session mode, multi-display, audio
> setup dock, scene build). Many new screens → many new mirror fields → churn-reduction codegen (A)
> earns its keep. Few → Shape C's orphan-test captures ~all the value and A is over-build.

This is exactly the judgment to spend the Fable window on: **is A worth its indirection, or does
C + a lint capture 90% of the value at 10% of the cost?** I lean C-first-then-decide-A; I'd want
Fable's kill-pass on that lean before it's a decision.

## 3. Invariants & enforcement (provisional — firms up once the fork resolves)

- **I1 — no orphan fields.** Every `ContentState` field has an emit site and a consume site.
  *Enforcement:* the Shape-C coverage test (ships first, fork-independent). Removes the
  `#[allow(dead_code)]`.
- **I2 — the hot-path packer is exempt and stays exempt.** `modulation_snapshot` is never routed
  through the projection mechanism. *Enforcement:* the exemption is a named list the mirror
  machinery excludes; a test asserts `modulation_snapshot`/`project_snapshot` are not table-driven.
- **I3 (Shape A only) — a mirror field that skips the declaration doesn't compile.** *Enforcement:*
  codegen is the single source of the field + its capture write.

## 4. Phasing (sketch — real briefs after the fork resolves)

- **P0 — orphan-coverage test + delete the 6 dead fields (Shape C).** Fork-independent, cheap,
  ships the verified win. Removes `#[allow(dead_code)]`. Gate: the coverage test is red before the
  deletions, green after; `rg` proves the 6 fields gone. *Vertical slice:* this is the whole path
  for one field-class — emit, consume, enforcement — proven once, thinly.
- **P1+ — declarative mirror (Shape A) IF Q-GROWTH says build it.** Table + codegen over the ~40
  mirror fields, exemption list for the other four classes. Deferred pending the fork.

## §. Decided — do not reopen
1. The hot-path `modulation_snapshot` packer is exempt from any projection mechanism (§0, I2).
2. Enforcement is scoped to the scalar-mirror class; events/snapshot/overlays are named exemptions,
   not absorbed (kill-test verdict).
3. This design does not claim to fix the stale-pixel bugs (BUG-015/060 etc.) — those are cache/A2
   (§1.2).

## §. Deferred
- Declarative mirror codegen (Shape A) — revive on Q-GROWTH = "many new screens."
- Folding A1 into FOUNDATIONAL_GAPS as resolved — at landing, per the STRUCTURAL_AUDIT_VERDICTS
  convention (keep the branch diff to the design files).
