# Landing: UI_FUNNEL_DECOMPOSITION P-S — surface split + RowHost dedup + catalog

**Date:** 2026-07-22 · **Branch:** `lane/ws1-surface` → main · **Executors:** ws1-ps-dispatch (Opus dispatcher) + 5 lanes (2 Opus semantic, 3 Sonnet pure-move) + read-only census seat. **Lander:** ws1 orchestrator. **Mode:** D-38 single-verification.

## What landed
- **P-S1/4/5 pure-move splits:** `param_slider_shared.rs` → 5-file, `param_card.rs` → 4-file, `panels/inspector.rs` → 4-file directory modules — every remaining Wave-1 god file is now a layer-bucketed directory module. All proven by move_identity (P-S5 residue-0; P-S1/P-S4 residues named and reviewed: sibling allowlist-test updates the splits force).
- **P-S2/3 RowHost dedup:** shared `RowHost` (id/routing machinery) extracted from ParamCardPanel; `SceneCardState` collapsed onto it — the scene panel's hand-copied twin (origin of the BUG-237/249/250/260 class) deleted, net −292 lines, hosts symmetric by construction.
- **P-S6a D9 catalog:** `cargo xtask ui-snap <scene> --catalog` enumerates every ParamSurface row affordance (durable id + RowRole + queryable name) over the existing dump — no new protocol; self-test makes a nameless row a red test (BUG-239 class kill). First run found BUG-302 (E/A arm buttons nameless — logged, batched with VERIFICATION_INFRA).
- **D10:** resolved per D-39 — CHROME_PARAMS designed-unification on the register; census's stale-thesis catch recorded in the P-S re-headline amendment.

## Gates (D-38: lanes' quoted runs + this single landing sweep)
Per-lane: quoted in each commit (clippy both flavors, nextest 1172-1173, renderer swatch target, scene/inspector flow subsets 17/17 + 22/22). Landing: full sweep + complete flow suite quoted in the push-time addendum below.

## Stage note
The inspector, the scene panel, and every card now run one shared machinery in small labeled files. The catalog means the test harness — and future agents — can ASK the app what's on screen instead of knowing by lore.

## Push-time addendum
Full sweep: `Summary [175.362s] 3852 tests run: 3852 passed (22 slow), 13 skipped`; clippy workspace clean; deny `bans ok`; flows **34/34 required, 7 xfail (all pre-existing known-red), 41/41 accounted**. Line-delta honesty (dispatcher's close report): repo-wide net **+287** — P-S was a SPLIT (structure + one shared host), not a deletion; the census's −500-800 projection applied only to the dedup half, which delivered (−280, scene twin dead).
