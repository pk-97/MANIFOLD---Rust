# FUSION_SOTA batch 2 (P4a + P5 + P4b) — landed 2026-07-14 @ <filled in post-merge>

**Branch:** feat/fusion-sota · **Level reached:** L2 / target L2 (§10 — P4a/P4b have a
user-visible-in-editor surface via `graph_tool fusion`'s dispatch-count delta, actually observed
and read; P5 is L1, a pure compiler-gate change with no independent user-visible surface)

**Doc status line (quoted verbatim):**
> **Status:** IN PROGRESS · 2026-07-14 · Fable 5 (with Peter in the room) · Sonnet 5 executing
> P1–P3 SHIPPED (markers module, segment worker robustness, refusal census committed as
> `docs/fusion_census.md` — no D4 default flipped, all four stand). P4a SHIPPED. **P5 SHIPPED**
> (Vec3 lift + D4 scope-expansion Vec4/Color lift, landed BEFORE P4b per the reordering below —
> `classify_node`'s param gate narrowed, fused codegen + install-time param seeding extended,
> `fusion_coverage_baseline` widened to effect+generator/flattened and its floor raised). **P4b
> SHIPPED** (closes BUG-114): the remaining five `draw_*` atoms + `blob_overlay` converted onto the
> codegen path; `BlobTracking.json` now forms a real 6-member fused region (18→13 estimated
> dispatches, `graph-tool fusion` measured). P6–P7 remain.

## Gate results (verbatim)

**P4a** (`ae9ab74c`) — `cargo test -p manifold-renderer --features gpu-proofs draw_dots` (via
`--lib`): 3 passed (incl. `generated_draw_dots_matches_hand_kernel`). Freeze suite: 72 passed, 0
failed, 3 ignored. Clippy clean. Mechanism proven at classify/region layer directly since the
draw_dots Color param independently blocked the literal region-formation demo at this point (the
escalation resolved by expanding P5's scope, below).

**Escalation from P4a, resolved with Peter (mid-wave):** all six `draw_*` atoms carry a `Color`
param; `classify_node`'s param gate rejected Vec3/Vec4/Color/Table/String uniformly for the fused
path, and FUSION_SOTA's original D4 only planned to lift Vec3. Without expanding scope, BUG-114
would close mechanism-only (no actual dispatch reduction). Peter's call: expand P5 to Vec4/Color
too (simpler than Vec3 — no padding needed) and reorder P5 before P4b. Recorded in
`docs/FUSION_SOTA_DESIGN.md`'s D4 and P4/P5 phase briefs (commit `21794f5c`).

**P5** (`1b013b0e`) — full gpu-proofs suite: 1548 passed, 8 failed (6 pre-existing `codegen::gpu_tests`
`BadInput` panics + 2 prewarm-cache tests) — orchestrating session independently re-verified all 8
against P4a HEAD in a throwaway detached worktree: the 6 codegen failures reproduce byte-identical
at P4a HEAD (pre-dating P5), and the 2 prewarm tests pass in isolation at P5 HEAD (the documented
full-suite-parallelism GPU flake per FREEZE_COMPILER_MAP.md §10, not a regression). Full `--lib`:
1219 passed, 0 failed. Clippy clean. Census: param-type refusals 19→10 (the 9 Vec3/Vec4/Color
flips). `fusion_coverage_baseline` widened (effect+generator, flattened) and its floor raised
32/52/203 → 32/54/216 regions/atoms.

**P4b** (`a2ca9b61`) — full gpu-proofs suite: 1556 passed, 8 failed (same 8 pre-existing, verified
identical before/after this phase). Full `--lib`: 1220 passed, 0 failed. Clippy clean. Docs-index
freshness: pass. **BUG-114 closed** — `docs/BUG_BACKLOG.md` Status → FIXED (orchestrating session
additionally reflowed the summary-index row into the Fixed section and applied the
`~~BUG-NNN~~ FIXED` convention, commit `d6330b1a` — `bug_status.py --check` was flagging drift
after the worker's `--write` moved only the detail section).

Full crate re-verification after P4b (orchestrating session): `cargo test -p manifold-renderer
--lib`: 1220 passed, 0 failed, 4 ignored. Freeze suite: 73 passed, 0 failed, 3 ignored. Clippy
(`-p manifold-renderer -- -D warnings`): clean.

Full workspace sweep at landing (this file's merge SHA, run in the main checkout):
<filled in below, post-merge>

## Deviations from brief

- P4a: could not deliver the literal `draw_dots_fuses_into_texture_region` demo per the original
  brief (blocked by the Color-param finding) — substituted a mechanism-level proof
  (`buffer_index_external_stays_external`, updated to show a real 2-member region via
  `partition_regions`) plus the before/after dispatch count showing 0 change (expected, pending
  P5). This is the escalation above, not an unreported gap.
- P5: scope expanded from "Vec3 only" to "Vec3 + Vec4/Color" per Peter's explicit decision
  (recorded in the design doc, commit `21794f5c`). Phase order changed: P5 now precedes P4b.
- P4b: `draw_scanlines` needed no `BufferIndex` tag (no array input) — was gated purely by the
  Color param P5 lifted. `draw_connections` proved the BufferIndex mechanism generalizes to TWO
  tagged array inputs on one atom, beyond what P4a's single-input proof covered.
- No other deviations.

## Shortcuts confessed (rolled up from phase reports)

- P4a: none.
- P5: none.
- P4b: none.
- Orchestrating session: fixed a bug-backlog index-reflow gap the P4b worker's `--write` didn't
  cover (table-row strikethrough convention) — not a shortcut, a landing-time correction.

## Verification debt

None opened. `draw_scanlines` staying unfused in `BlobTracking.json` is NOT verification debt —
it's a genuine, expected non-fusion (topologically isolated by two `value_overlay` draw-call
boundaries in that specific preset, not a param/array gap), documented in BUG-114's fix writeup.

## Click-script for Peter (≤2 minutes)

1. `cat docs/BUG_BACKLOG.md` (search BUG-114) — expect: Status FIXED, in the Fixed section, with
   the measured 18→13 dispatch drop on `BlobTracking.json`.
2. Open `BlobTracking.json`'s effect/generator in the editor (whichever project uses it) — expect:
   no visual change (fusion never changes pixels, only dispatch count) — the demo here is
   performance headroom, not appearance.
3. `cargo test -p manifold-renderer --lib` — expect: all green (1220 tests).
