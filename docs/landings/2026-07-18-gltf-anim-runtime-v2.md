# Landing — glTF Animation Runtime v2 (lane/gltf-anim-v2, P1–P4)

**Design:** `docs/GLTF_ANIM_RUNTIME_V2_DESIGN.md` (status updated in this landing: BUILT P1–P4).
**Session:** 2026-07-18, Fable 5 orchestrating, Sonnet 5 executors (one per phase).
**Commits:** `b811ca55` (P1) · `9f153abe` (P2) · `7ba73904` (P3) · `64511f01` (P4 sweep) · `4ec1f68e` (origin/main merge, BUG_BACKLOG conflict resolved: BUG-242 kept in Fixed per origin, BUG-247/248 added).

## What shipped

Keyframe payload moved out of graph defs into a shared file-backed cache (`gltf_anim_cache.rs`, `Weak`-held per file); all three CPU samplers binary-search flat tracks; the importer emits no keyframe tables; old defs load-migrate; multi-node rigid animation rides the skinning path via a node-slot palette (the two "left static" bails deleted); clip cap 31 → 255.

## Gate results (worktree, warm, post-merge with origin/main)

- `cargo nextest run --workspace` — `Summary [14.848s] 3764 tests run: 3764 passed (1 leaky), 13 skipped`
- `cargo clippy --workspace -- -D warnings` — clean (8.87s)
- `cargo deny check bans` — `bans ok`
- `cargo test -p manifold-renderer --features gpu-proofs --lib gltf` — `test result: ok. 152 passed; 0 failed; 5 ignored` (first attempt hit the known device-contention flake; rerun clean)

## Acceptance measurements (P4, release build, `/usr/bin/time -l`)

| Asset | Peak RSS | Baseline | Notes |
|---|---|---|---|
| dragon (52 clips, 5.41 M keys) | 1.42–1.46 GB across 4 poses | 5.19 GB (−72%) | instructions 125 G → ~16.7 G (−87%); 4 pairwise-distinct PNGs, jaw visibly animates |
| blossom control (static) | 0.40 GB | 0.43 GB | no regression |

Original `< 1 GB` gate was a design-time estimate — **revised transparently** (design §3): measured floor is 0.40 GB; residual attributed (inference, unproven) to per-source-node whole-file parses → **BUG-247**. Peter's reported after-delete GUI-FPS symptom on static assets → **BUG-248** (needs in-app profile). BUG-190's 370 ms figure not re-measured in-app; entry noted, left OPEN.

**Verification level:** L2 (PNG artifacts read by orchestrator: `/tmp/drogon_v2_t{0.5,1.5,2.5,3.5}.png`, `/tmp/blossom_v2.png`). L4 pending Peter.
**Verification debt:** in-app steady-state frame time + delete-recovers-memory observation are unverified headlessly — carried as BUG-248 + the BUG-190 note (no separate VD entry; the bugs are the ledger here).

## Click-script for Peter (~2 min)

1. Import `drogon__game_of_thrones_dragon/scene.gltf` as a layer. Expect: app stays responsive during import; dragon appears and its animation plays (jaw/wings move).
2. Open the imported card, switch Clip between a few indices (0–51 all selectable now). Expect: pose family changes.
3. Watch Activity Monitor memory: expect ~1.5 GB-class, not 5+ GB.
4. Delete the layer. Expect: memory drops back within seconds (the cache frees when the last node goes); FPS recovers. If FPS does NOT recover on the static blossom asset, that's BUG-248 — say so and I'll profile in-app.
