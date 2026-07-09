# Landing — PARAM_STORAGE_BOUNDARIES P3

**Date:** 2026-07-09 · **Design:** [PARAM_STORAGE_BOUNDARIES_DESIGN.md](../PARAM_STORAGE_BOUNDARIES_DESIGN.md) · **Phase:** P3 (migration reads `embeddedPresets`, fixes BUG-040)
**Branch:** `wave/param-boundaries-p3` @ `eec807cd` → merged `--no-ff` into `main`
**Orchestrator:** Opus (X-High); **executor:** Sonnet worker (medium)

## What shipped
The V1.3→V1.4 param-storage migration now consults the file's OWN `embeddedPresets` for positional→id order before falling back to the baked `LEGACY_PARAM_ORDER` table. Fixes **BUG-040**: an imported/project-local generator TRACKING instance has `graph: None` and isn't in the frozen table, so its positional params were silently dropped on load of any project saved in the ~1-day V1.3→V1.4 window. New read-only `embedded_param_orders(root)` (built once per `migrate()`, threaded as `&HashMap<String, Vec<String>>` — never touches the live registry, honoring the migration quarantine). Order authority chain: own inline graph → file's `embeddedPresets` → baked table → loud-drop.

## Gate (orchestrator re-ran independently)
- `cargo test -p manifold-io`: **green** — 40 lib (incl. the 3 new BUG-040 fixtures: with-matching-preset resolves by that order, without falls to baked table, own-graph-order still wins), 4 history_snapshots, 15 load_project.
- Negative grep `preset_definition_registry` functional use in `crates/manifold-io/src/migrations/`: **zero** (the one lexical hit at `param_storage_v14.rs:33` is a pre-existing doc-comment, not code — confirmed by `rg "use .*|::"` → zero).
- `cargo clippy --workspace -- -D warnings`: clean.

## Full-workspace sweep: RED, but provably external to this wave
P3 owns the wave's single `cargo test --workspace`. It reports **987 passed / 6 failed**. All 6 failures are `manifold-renderer::ui_cache_manager::tests`, each panicking at `manifold-ui/src/tree.rs:290` on the UI_CLIP_AND_Z_OWNERSHIP D1/D4 region-ownership assertion. **Orchestrator independently reproduced all 6 on `main` at `10251041` (P1 landed, P3 NOT merged) — identical 0-passed/6-failed.** The cause is `0bb51dad` ("region mechanism … D4 enforcement", 2026-07-08), confirmed an ancestor of `10251041`, predating the entire param-boundaries wave. P3 touches only `param_storage_v14.rs` + two docs (`git diff --stat`). Landed over the external red per repo precedent (BUG-072/074 logged-not-blocked). Logged as **BUG-077** (LOW); the fix is mechanical — wrap the 6 test fixtures' node minting in `begin_region`/`end_region` per the shipped D4 contract. A follow-up restores the trunk's default sweep to green.

## Verification level: **L1** (pure data migration; the 3 fixtures are the artifact) — P3's target was L1, so **no verification-debt entry** (the red sweep is a pre-existing external bug, not a gap in P3's own verification; tracked as BUG-077 in the backlog, follow-up actioned same day).

## Peter click-script (~1 min)
1. Open a project saved in the V1.3→V1.4 window that contains imported generators (or any older project with project-local generators). → Expect: their params now load intact instead of dropping.
There is no new UI; this is a load-fidelity fix for old files.

## Deviations from the brief: none. Deliverables exactly as §4 P3 specifies; the negative-grep lexical hit was flagged, not worked around (editing pre-existing out-of-scope prose was correctly declined).

## Status line (quoted verbatim; P2 slot reconciled at P2's land)
`**Status:** IN PROGRESS · P1 SHIPPED (\`wave/param-boundaries-p1\`) · P3 SHIPPED (\`wave/param-boundaries-p3\`) · P2 not built · 2026-07-06 · Fable`
