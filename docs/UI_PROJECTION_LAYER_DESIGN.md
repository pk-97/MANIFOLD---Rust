# UI ↔ Content Projection Layer — the A1 kill-test, run to completion

**Status:** SHIPPED (P0, the only surviving phase) · 2026-07-09 · Fable. The A1 kill-test came

> **Supersession note (2026-07-22, UI_FUNNEL P-Z):** references below to `dispatch_inspector` / `ActiveInspectorDrag` / `PanelAction` trio variants describe the PRE-decomposition architecture. Current state: 12 flat domain enums + exhaustive router (P-D), one Scrub gesture wire with `ScrubState.active` (P-I, `ActiveInspectorDrag` extinct), per-domain `dispatch/` handlers (P-B). Anchors here are historical.
back **KILL** for the declarative projection layer; the surviving piece — compiler-enforced
orphan coverage — landed the same session. This reverses the pre-Fable draft's C-then-A
recommendation (`0271934b`); Peter may override — §2 names the trigger that would revive the layer.
**Prerequisites:** none. UI_HARNESS_UNIFICATION (approved, Sonnet-executing) is the
*verification* half of the seam story and is unaffected.
**Execution contract:** nothing left to execute. This doc is the record that stops the
declarative layer being re-proposed without new evidence — read §2 before reopening.

**What happened.** FOUNDATIONAL_GAPS A1 pre-registered a kill-test: survey the actual snapshot
fields; kill the declarative layer if it would be mostly escape hatches. The draft surveyed
field **classes**, found four bespoke classes, scoped the table to the regular scalar-mirror
class, and answered scoped-yes. This pass surveyed each field's **halves** and inventoried
existing enforcement (§1.1): the emit half is already compiler-enforced, the consume half is
bespoke display logic no table can generate. The table would generate the only part that never
breaks. Kill.

## 0. Binding constraints (unchanged from the draft — re-confirmed)

- **Thread residency.** Content thread owns `Project`; UI sees `ContentState` snapshots on a
  bounded channel; commands go the other way. Conform, never renegotiate.
- **Hot path.** `ContentState.modulation_snapshot` (zero-alloc flat-buffer packer, D8 topology
  guards) must never route through a generic mechanism. Held trivially: no mechanism exists.

Persistence and time-model do not bind (`ContentState` is transient, never serialized).

## 1. Audit — what exists (verified 2026-07-09, Fable; supersedes the draft's numbers)

- **Emit:** [content_state.rs](../crates/manifold-app/src/content_state.rs) — `ContentState`,
  **56 fields post-P0** (66 pre; the draft said ~70), built as an **exhaustive struct literal**
  at [content_thread.rs:1161](../crates/manifold-app/src/content_thread.rs#L1161). Export-path
  literals spread from `Default` deliberately (degraded keep-alive snapshots).
- **Apply:** [ui_bridge/state_sync.rs](../crates/manifold-app/src/ui_bridge/state_sync.rs)
  (2457 lines) — `push_state` :173, `sync_project_data` :850, `sync_inspector_data` :1234.
- **Boundary:** [ui_translate.rs](../crates/manifold-app/src/ui_translate.rs) (673 lines).
- **Churn, recounted:** 191 commits on state_sync.rs since March, 35 with fix/bug/stale
  subjects. Composition (sampled): inspector/view-model display semantics (`sync_*_data`
  territory) — **not** scalar-mirror threading.
- **Field classes (counts corrected):** scalar mirror ~49 · gated overlay 13 (`spectrogram_*` 8
  + editor/graph 5) · one-shot events 2 · `project_snapshot` 1 · `modulation_snapshot` 1.
- **Orphans: 10, not 6.** The FIXME named 6 (and 3 `stem_*` fields that no longer existed —
  the note itself had rotted). When P0 un-suppressed the lint, it found 4 more the draft's
  survey missed: `is_exporting`, `export_progress`, `export_status`,
  `recording_dropped_frames`. Git shows the last four **never had a consumer** — video export
  has no progress display and never did (BUG-083), the recording drop counter was never
  surfaced (BUG-084).

### 1.1 The enforcement inventory that flips the verdict (each claim observed, not derived)

1. **Emit is compiler-enforced.** The snapshot is an exhaustive struct literal — a
   declared-but-never-written field cannot compile.
2. **Orphan enforcement already existed in the compiler.** `manifold-app` is a bin crate, so
   rustc's `dead_code` lint sees every read site; the struct-level `#[allow(dead_code)]` was
   suppressing exactly what the draft's Shape C proposed to build as a new test. Proof:
   `cargo rustc -p manifold-app --bin manifold -- --force-warn dead_code` names precisely the
   orphans and skips every read field.
3. **Drag suppression already exists, generically:**
   [`ActiveInspectorDrag`](../crates/manifold-app/src/app.rs) (app.rs:52) — its `Param` variant
   covers *any* param via `GraphTarget` + `ParamId`, applied after snapshot acceptance. Its
   targets are **`Project` fields riding the project/modulation snapshots, not `ContentState`
   mirrors**. The draft's flagship `mirror!` example (`master_opacity`) is not a
   `ContentState` field.
4. **Per-field dirty tracking has no substrate.** `push_state` re-pushes every UI frame
   through display caches (`TransportDisplayCache`); `dirty: on_change` declared a concept the
   mechanism doesn't have or need.

And the consume half itself (read `push_state` :186–:402): play-state colors, BPM authority
arbitration, three-state Link/CLK/SYNC displays, edge-trigger toast guards — bespoke
presentation per field, all the way down. That is the escape-hatch tell, inside the very class
the table was scoped to.

### 1.2 Bug evidence (draft's correction confirmed, one fix)

BUG-026 is a missing animation poll; BUG-036 is load-ordering — not projection. Fix to the
draft's wording: BUG-015 is **OPEN** (repro needed) and BUG-060 is **REOPENED** (live path via
`panel_cache_info()`/`UICacheManager`, cause open) — not "already fixed/addressed". Both are
A2/cache territory. **Corpus bugs in the class the mirror table would govern: zero.**

## 2. The verdict, priced

| `mirror!` table would buy | Reality (observed) |
|---|---|
| I1: no orphan fields | Already in the compiler; suppressed by one attribute. Shipped as P0. |
| I3: undeclared field can't reach UI | No observed bug in class; emit literal-checked, consume lint-checked. |
| Drag suppression per field | Exists, generic, at a different seam (project snapshots). |
| Dirty conditions per field | No per-field dirty machinery exists to declare into. |
| Kills hand-threading churn | The churn is display/view-model semantics a table can't generate. |
| Kills a stale-state bug class | Zero corpus bugs in the mirror class (§1.2). |

Cost side (from the draft, still true): an indirection layer, likely codegen, generated code in
the tree, ceremony per field.

> **Q-GROWTH — answered (Peter, 2026-07-09):** *"Many Many new screens pages and interactive
> new UI is coming soon."*

The fact stands; the inference does not. A new screen hand-writes view-model translation and
display logic (table can't help), gets param drags free (`ActiveInspectorDrag::Param`), and a
new mirror field costs one struct line + one literal line, both compiler-checked. **The
multiplier is real; the multiplicand is approximately zero.**

**Rejected (for the future session that will reinvent these at 2am):**
- **Shape A — declarative mirror table**, all §2.2 realizations (proc-macro / build.rs /
  registry; that fork is moot): every claimed benefit dissolves against §1.1/§1.2.
- **Shape B — `Projected<T>` read-tracking:** a runtime, test-dependent version of what the
  `dead_code` lint does statically, for free.
- **Shape C as a new test:** inventing infra that exists — the lint IS the coverage test; the
  gate is the existing `cargo clippy --workspace -- -D warnings`.

**Reviving trigger, stated honestly:** a real bug class in mirror threading — say three backlog
bugs whose root cause is a mis-threaded mirror field. Today's count: zero.

**The one live residual:** *source-arbitration* bugs — which side of the seam is authoritative
for a field while the user edits it. Two observed, both fixed: BPM (`101616e4`, `7a946218`;
the worked pattern is the arbitration comment at state_sync.rs:202) and VSync (`0da7f99e`).
Per-field semantic judgment — no table or lint checks it. n=2; UI_HARNESS_UNIFICATION's L3
flows are its observability net. Third bite → fold the pattern into `ui-state-sync-path`.

## 3. Invariants & enforcement

- **I1 — no orphan fields.** Emit: exhaustive struct literal (compile error). Consume: rustc
  `dead_code`, live since P0 removed the allow, failing the existing pre-commit
  `cargo clippy --workspace -- -D warnings` gate. A field staged "for later" fails by design —
  land the field WITH its consumer, or don't land it. Feature-gated consumers only count under
  their feature: the A7 feature-matrix clippy job (STRUCTURAL_AUDIT_VERDICTS §3) is what makes
  I1 sound across the matrix.
- **I2 — the hot-path packer stays bespoke.** No mechanism exists to route it through; this
  line is the record of why one must not be built.

## 4. Phasing

- **P0 — orphan purge + un-suppression. SHIPPED 2026-07-09 (Fable).** Deleted: all 10 orphan
  fields, their emit writes, and the write-only `cached_osc_timecode` cache the deletion
  orphaned in turn (field, maintenance block, two initializers); `send_export_progress`
  demoted to a documented transport keep-alive; stale FIXME and struct-level
  `#[allow(dead_code)]` removed — the struct doc-comment now names the enforcement and forbids
  re-adding the allow. Gate (run this session): workspace clippy `-D warnings` green WITH the
  allow gone; `cargo test -p manifold-app` green (163+10+1+3); negative `rg` for the field
  names in `manifold-app/src` zero (the one repo hit left is `LinkSync`'s own `link_tempo` —
  a different struct). Demo: none — L1; the observation was the lint firing pre-deletion
  (§1.1 proof command).
- **No further phases.** P1 (the table) killed per §2.

Side-note from the probe: three more never-read fields exist behind their own allows
(`input_host.rs:34` cfg-gated, `project_io.rs:185 flash_save`, `workspace.rs:42 kind`) — each
names its un-suppression trigger per the CLAUDE.md rule. Sanctioned pending work, not orphans.

## §. Decided — do not reopen

1. **The declarative mirror layer is KILLED** by A1's own kill-test (§1.1/§2). Reviving
   trigger in §2. Reverses the pre-Fable draft's C-then-A; Peter may override.
2. `modulation_snapshot` is exempt from any projection mechanism, current or future.
3. This design never claimed the stale-pixel bugs: BUG-015 (OPEN) / BUG-060 (REOPENED) are
   A2/cache — UI_CLIP_AND_Z_OWNERSHIP_DESIGN + UI_HARNESS_UNIFICATION.
4. Q-GROWTH's fact is settled and does NOT revive the table (§2).

## §. Deferred

- **Export progress display (BUG-083)** and **recording drop counter (BUG-084)** — the product
  gaps P0's lint exposed. When built, their fields return WITH consumers (I1 enforces this).
- **Source-arbitration write-up** — trigger: a third bug in the class (§2 residual).
