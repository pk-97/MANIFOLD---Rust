# Bug Corpus Dossier — mined for structural-verdict pass

Miner: Sonnet (background job), 2026-07-09. Read-only mining; classification below is a
**suggestion**, not a verdict — a later high-tier session issues verdicts per subsystem.

## §1 Method + inputs

Sources: `docs/BUG_BACKLOG.md` (2898 lines, BUG-001–082, full read, no gaps), `docs/FOUNDATIONAL_GAPS.md`
(clusters A1–A7 + Part B), `docs/CORE_ENGINE_FINDINGS.md` (findings F1–F17, all `manifold-playback`),
and `git log --grep='fix' -i` (1672 commits) with a patch-density map + sampled diffs on the top ~10
files. Extraction of raw bug fields and git mining were delegated to three parallel read-only agents
(BUG_BACKLOG extraction, git patch-density, FOUNDATIONAL_GAPS/CORE_ENGINE_FINDINGS summarization);
the taxonomy classification, cluster cross-referencing, subsystem rollup, and suggested verdicts
below are mine, done against that raw material. The "Checked and safe" section (line 2866) contains
**zero** numbered bugs — it lists 4 audited-and-found-correct duplication paths, confirmed by direct
read; no bug in §2 below carries `checked-safe` status as a result. Cross-reference `rg` over the
full backlog for `FOUNDATIONAL_GAPS|CORE_ENGINE_FINDINGS|\bF1[0-7]?\b|\bA[1-7]\b` found **zero**
hits — the backlog never names either doc's clusters by id; all cluster mappings below are mine,
inferred from subsystem + mechanism match, not doc-stated.

## §2 Per-bug table (all 82)

Columns: ID | Subsystem | Class | Fix | Enforcement | Evidence | Cluster. Fix: `struct`=fixed-structural,
`patch`=fixed-patch, `open`=open (parenthetical = doc's own status note). Cluster: `none` = no fit found
in FOUNDATIONAL_GAPS/CORE_ENGINE_FINDINGS (candidate for §5).

| ID | Subsystem | Class | Fix | Enforcement | Evidence | Cluster |
|---|---|---|---|---|---|---|
| BUG-001 | manifold-editing/clipboard.rs | identity-minting-on-duplicate | struct | none named | BUG_BACKLOG.md:2748 | A5 |
| BUG-002 | manifold-core/clip.rs | identity-minting-on-duplicate | struct | none named | :2784 | A5 |
| BUG-003 | manifold-core/effects.rs+layer.rs | identity-minting-on-duplicate | struct | none named | :2820 | A5 |
| BUG-004 | manifold-editing/clipboard.rs | structural-design-flaw | patch | none named | :2834 | A4 |
| BUG-005 | manifold-core/macro_bank.rs | missing-invariant-enforcement | struct | none named | :2851 | A5 |
| BUG-006 | manifold-renderer/node_graph/bound_graph.rs | stale-state/projection | open | none | :953 | none |
| BUG-007 | manifold-renderer/node_graph/freeze/region.rs | missing-invariant-enforcement | open | none | :975 | none |
| BUG-008 | manifold-renderer/node_graph/freeze/codegen.rs | missing-invariant-enforcement | open | none | :998 | none |
| BUG-009 | manifold-renderer/node_graph/freeze/segment.rs | missing-invariant-enforcement | open | none | :1017 | none |
| BUG-010 | manifold-renderer/node_graph/primitives/wgsl_compute.rs | missing-invariant-enforcement | open | none | :1041 | none |
| BUG-011 | manifold-renderer/node_graph/primitives/wgsl_compute.rs | missing-invariant-enforcement | open | none | :1060 | none |
| BUG-012 | manifold-renderer/node_graph/primitives/wgsl_compute.rs | missing-invariant-enforcement | open | none | :1078 | none |
| BUG-013 | manifold-gpu/metal/encoder.rs | resource-lifecycle | struct | verify_completed gate | :2198 | none |
| BUG-014 | manifold-renderer/node_graph/freeze/install.rs | convention-mismatch (serde) | open (parked) | none | :1093 | none |
| BUG-015 | manifold-renderer/ui_cache_manager.rs + inspector.rs | stale-state/projection | open (partial) | 2 tests named | :1107 | A1 |
| BUG-016 | manifold-renderer/gltf_import.rs + app_lifecycle.rs | structural-design-flaw | struct | none named | :2233 | none |
| BUG-017 | docs/tooling (docs_index_sync test) | process-failure | patch | docs_index_is_in_sync_with_docs_dir | :2265 | A7 (adjacent) |
| BUG-018 | manifold-renderer/node_graph::catalog_gen | process-failure | open | catalog_gen::tests::regenerates_in_sync | :1476 | A7 (adjacent) |
| BUG-019 | manifold-ui (inspector, EffectGroup) | one-off | open (deferred) | none | :1494 | none |
| BUG-020 | manifold-ui/param_card.rs | structural-design-flaw | open (deferred) | none | :1511 | none |
| BUG-021 | manifold-app/ui_bridge/inspector.rs | structural-design-flaw | open (deferred) | none | :1522 | none |
| BUG-022 | manifold-app/window_input.rs | missing-invariant-enforcement | patch | none named | :2287 | A3 |
| BUG-023 | manifold-ui/design_tokens.rs+browser_popup.rs | convention-mismatch (tokens) | patch | no_new_raw_color_literals | :2369 | none |
| BUG-024 | manifold-renderer/preset_thumbnail.rs | convention-mismatch (alpha) | patch | none named | :2329 | none |
| BUG-025 | manifold-ui timeline (clip/header scissor) | stale-state/projection | open (unreproduced) | none | :1533 | A2 |
| BUG-026 | manifold-ui/browser_popup.rs+app_render.rs | stale-state/projection | open (fix landed, unverified) | none named (VD-006) | :1570 | A1 |
| BUG-027 | manifold-ui/manifold-app graph editor | structural-design-flaw | struct | node_previews_render_in_per_node_depth_bands | :2404 | A2 |
| BUG-028 | manifold-app/drag_interpose.rs+app.rs | missing-invariant-enforcement | struct | 4 unit tests | :2455 | A3 |
| BUG-029 | manifold-app/content_thread.rs+content_commands.rs | process-failure | patch | cargo check --features profiling gate | :2144 | A7 |
| BUG-030 | manifold-ui/design_tokens.rs | convention-mismatch (tokens) | open (parked) | design_tokens.rs ratchet | :924 | none |
| BUG-031 | manifold-ui/layer_header.rs+app.rs | missing-invariant-enforcement | open | none | :1599 | A5 |
| BUG-032 | manifold-renderer/node_graph graph_loader | structural-design-flaw | struct | 2 tests named | :2496 | none |
| BUG-033 | manifold-app/ui_snapshot/interact.rs | process-failure | patch | none named (fixed incidentally) | :2174 | A7 |
| BUG-034 | manifold-app/app_render.rs (atlas UV) | process-failure | open (gated on BUG-033) | none | :908 | A7 (adjacent) |
| BUG-035 | manifold-app/content_pipeline.rs | structural-design-flaw (hot-path) | open (root cause found, no fix) | none | :780 | none |
| BUG-036 | manifold-io/loader.rs+manifold-core/effects.rs | structural-design-flaw | struct | project_local_preset_reload.rs | :2090 | A1 |
| BUG-037 | manifold-renderer generator pipeline warm-up | resource-lifecycle | open | none | :750 | none |
| BUG-038 | manifold-playback/ableton_bridge.rs | missing-invariant-enforcement | open | none | :769 | F11 (loose) |
| BUG-039 | manifold-core ParamSpecDef/modulation | convention-mismatch (units) | open (sequenced) | none (planned) | :723 | none |
| BUG-040 | manifold-io/migrations/param_storage_v14.rs | structural-design-flaw | patch | 3 bug040_* tests | :2532 | B1 |
| BUG-041 | manifold-audio/analysis.rs (SuperFlux) | one-off | patch | mod_harness selftest gates | :2706 | none |
| BUG-042 | manifold-audio D5 ridge tracker | one-off | struct | notes accuracy/octave-jump gates | :2632 | none |
| BUG-043 | manifold-audio salience comb | one-off | struct | sub scenario gate (100/100) | :2664 | none |
| BUG-044 | manifold-audio transient/onset detection | one-off | struct | densemix selftest gate (7/7) | :2591 | none |
| BUG-045 | manifold-audio D5 tracker | one-off | open (declined, knife-edge) | P2c notes accuracy (known-failing) | :699 | none |
| BUG-046 | manifold-audio Low-band kick detection | one-off | open (partial) | mod_harness recovery counts | :644 | none |
| BUG-047 | manifold-ui/audio_setup_panel.rs | structural-design-flaw | open | consumers_fit_within_panel test | :626 | A2 |
| BUG-048 | manifold-ui/transport.rs | one-off (UX) | open (UX call pending) | automation_state_toggles test | :611 | none |
| BUG-049 | manifold-ui/layer_header.rs | one-off | open | layout_matches_frozen_oracle (stale) | :598 | none |
| BUG-050 | manifold-playback/transport_sync.rs | structural-design-flaw | open (partial) | [ABL-SYNC] traces + 3 tests | :574 | F6/F14 (loose) |
| BUG-051 | manifold-playback/live_trigger.rs+modulation.rs | missing-invariant-enforcement | struct | clear_all_trigger_edges_rearms_generator_edge | :2570 | none |
| BUG-052 | manifold-audio/manifold-playback | convention-mismatch (units) | struct | time_grid_holds_hop_and_window_duration | :1925 | none |
| BUG-053 | manifold-media/recording (session.rs+native) | structural-design-flaw | open | hdr_blocked_by_bug_053 guard | :553 | none |
| BUG-054 | manifold-renderer/generator_renderer.rs | resource-lifecycle | open | none (workaround only) | :532 | A6 |
| BUG-055 | manifold-audio examples | convention-mismatch (units) | patch | debug_assert grid match | :2074 | none |
| BUG-056 | manifold-playback/audio_mixdown.rs | process-failure | open | none | :493 | A7 |
| BUG-057 | manifold-app/ui_snapshot/render.rs | process-failure | open | none | :513 | A7 |
| BUG-058 | manifold-app/ui_root.rs | missing-invariant-enforcement | struct | DRAG_CAPTURE P1-P3 tests | :1963 | A3 |
| BUG-059 | manifold-ui/audio_setup_panel.rs | missing-invariant-enforcement | struct | 2 tests named | :2012 | A3 |
| BUG-060 | manifold-ui/inspector.rs+ui_cache_manager.rs | stale-state/projection | open (REOPENED) | footer_leak_probe test | :1309 | A1 |
| BUG-061 | manifold-ui/slider.rs+param_card.rs | missing-invariant-enforcement | struct | per-surface SliderReset tests | :1689 | none |
| BUG-062 | manifold-io/migrate.rs | missing-invariant-enforcement | struct | forward-version guard / LoadError::TooNew | :1626 | B1 |
| BUG-063 | manifold-io/loader.rs | missing-invariant-enforcement | open (P3 partial) | non-blocking toast only | :293 | B1 |
| BUG-064 | manifold-io/archive.rs | resource-lifecycle | patch | 2 sync_all negative-gate | :1651 | B1 |
| BUG-065 | manifold-io/archive.rs | structural-design-flaw | patch | none named | :1671 | B1 |
| BUG-066 | manifold-renderer node_graph FluidSim3D | one-off | open | fluid3d_bias.rs (--ignored) | :182 | none |
| BUG-067 | manifold-app/ui_snapshot/render.rs | process-failure | open | none | :161 | A7 |
| BUG-068 | manifold-app ui_snapshot fixtures | one-off | open | none | :172 | none |
| BUG-069 | licensing (deps: madmom/ADTOF, rusty_link, ffmpeg) | other: dependency-licensing | open | rg zero-hit gate (planned) | :115 | none |
| BUG-070 | manifold-ui/audio_setup_panel.rs+drawer.rs | missing-invariant-enforcement | open (partial) | BUG-061 tests (partial coverage) | :461 | none |
| BUG-071 | manifold-app/ui_snapshot/dump.rs | stale-state/projection | open | none | :110 | none |
| BUG-072 | manifold-playback/audio_mixdown.rs | process-failure | open | none | :440 | A7 |
| BUG-073 | manifold-app ui_snapshot --script driver | process-failure | open (workaround only) | none | :397 | A7 |
| BUG-074 | manifold-playback/audio_mixdown.rs tests | resource-lifecycle | open (unknown root cause) | none | :376 | none |
| BUG-075 | manifold-app/ui_root.rs | missing-invariant-enforcement | patch | timeline_drag_end_reaches_viewport test | :1894 | A3 |
| BUG-076 | manifold-ui/inspector.rs (ScrollContainer) | stale-state/projection | open (unconfirmed) | none (planned test) | :334 | none |
| BUG-077 | manifold-renderer+manifold-ui test fixtures | process-failure | patch | cargo test --workspace (0 hits) | :1799 | none |
| BUG-078 | manifold-renderer/preset_runtime.rs | stale-state/projection | struct | generator_rebuild_reshape_honors_live_manifest | :1744 | none |
| BUG-079 | manifold-core/effects.rs+preset_runtime.rs | missing-invariant-enforcement | open | none | :105 | none |
| BUG-080 | manifold-core param manifest construction | structural-design-flaw | open (wants Opus design pass) | none | :100 | none |
| BUG-081 | manifold-playback/audio_layer_playback.rs | one-off | open | none | :887 | none |
| BUG-082 | manifold-playback/modulation.rs+param_slider_shared.rs | structural-design-flaw | open | none | :95 | none |

Class totals (n=82): missing-invariant-enforcement 20 · structural-design-flaw 15 · one-off 12 ·
process-failure 11 · stale-state/projection 8 · convention-mismatch 7 · resource-lifecycle 5 ·
identity-minting-on-duplicate 3 · other 1.

Fix-type totals: fixed-structural 20 · fixed-patch 14 · open 48 · checked-safe 0 (confirmed §1).
**48/82 (59%) are still open** — the corpus is not a closed record, it's a live backlog.

## §3 Per-subsystem rollup (SUGGESTED verdicts — miner opinion, pending review)

| Subsystem | n | Open | Class histogram (top) | Enforcement | Suggested verdict |
|---|---|---|---|---|---|
| manifold-renderer/node_graph/freeze (fusion compiler) | 8 (006–012,014) | 8/8 (100%) | missing-inv×6, stale-state×1, convention×1 | **none, on any of them** | **structurally-wrong** |
| manifold-playback | 8 (038,050,051,056,072,074,081,082) | 7/8 (88%) | structural×2, missing-inv×2, process×2 | mostly none; also owns all 17 F-findings (F1–F17, all OPEN) | **structurally-wrong** |
| manifold-ui | 16 | 12/16 (75%), 1 reopened | stale-state×4, missing-inv×4, structural×3 | thin; concentrated on the 4 fixed-structural items | **structurally-wrong** (stale-state/projection slice) |
| manifold-app | 14 | 8/14 (57%) | process×6 (feature-matrix rot), missing-inv×4 | scattered | sound-but-underspecified |
| manifold-core | 6 | 3/6 (50%) | identity×3(fixed), structural×1(080), convention×1 | good on identity (3 tests-shaped fixes); none on BUG-080 | sound-but-underspecified (BUG-080 area alone: structurally-wrong) |
| manifold-io | 6 | 1/6 (17%) | missing-inv×3, resource×1, structural×1, structural×1 | strong: forward-version guard, ceiling guard, fsync gate, 3 migration tests | sound-but-underspecified |
| manifold-audio | 8 | 2/8 (25%) | one-off×8 (algorithm tuning) | strong: accuracy-gate tests per fix | sound |
| manifold-editing | 2 | 0/2 | structural×1, identity×1 | none named, but n=2 | sound (low confidence, n=2) |
| manifold-gpu | 1 | 0/1 | resource-lifecycle | verify_completed gate | sound (low confidence, n=1) |
| manifold-media/recording | 1 (053) | 1/1 | structural (HDR pipeline can't work) | guard test only | structurally-wrong (severe, n=1 — feature is fully blocked, not degraded) |
| docs/tooling | 1 (017) | 0/1 | process | test named | sound (n=1) |
| licensing (cross-crate) | 1 (069) | 1/1 | other | planned gate | not applicable — legal/dependency risk, not an architecture verdict |

Notes on the three flagged **structurally-wrong**:

1. **Freeze/fusion compiler** — BUG-006 through BUG-014 is one campaign (8 bugs found together,
   same day range per doc line clustering) that found silent no-ops, OOB reads, silent StateStore
   resets, wrong-entry-point dispatch, ghost instances from oversized buffers, and NaN/Inf hash
   collisions in the freeze/fusion path — and shipped **zero** fixes and **zero** enforcement for
   any of them. `docs/FREEZE_COMPILER_MAP.md` already claims a "precision contract" and "executor
   invariants" as authoritative; this 8-bug campaign is the concrete counter-evidence that the
   contract isn't enforced in code, only documented.

2. **manifold-playback** — worst backlog fix ratio of any crate with n≥3 (1/8), *and* it independently
   carries all of `docs/CORE_ENGINE_FINDINGS.md`'s F1–F17, every one still OPEN, including F5
   ("`clock_authority` + `bpm` mutated per-frame outside `EditingService`, bypassing
   `data_version`/undo") which is a direct violation of this repo's own hard rule ("All mutations
   through EditingService") and is marked DECISION NEEDED rather than fixed. Two crate-owned bugs
   (056, 072) are the same clippy-gate-drift pattern recurring twice on the same file
   (`audio_mixdown.rs`) without ever being closed.

3. **manifold-ui stale-state/projection slice** — 4 of the 8 corpus-wide stale-state/projection bugs
   live here (015, 026, 060, 076), one (060) is explicitly REOPENED after a prior fix, and
   `FOUNDATIONAL_GAPS.md`'s own A1 cluster independently names the root cause ("No enforcement layer
   for UI/content snapshot sync; every field hand-threaded") — corpus evidence and existing
   structural-audit language agree, which is why this reads as structurally-wrong rather than merely
   under-tested.

## §4 Patch-density findings from git history

`git log --grep='fix' -i` over current-branch history: **1672 commits**, ~1.1% noise
(fixture/prefix/suffix false-positives). Top of the density map (fix-labeled commits touching path):
`app.rs` 165, `app_render.rs` 134, `content_thread.rs` 86, `ui_bridge/state_sync.rs` 80,
`node_graph/primitives/mod.rs` 71 (grep artifact — decomposition-tranche commits, not bugs),
`ui_root.rs` 68, `content_pipeline.rs` 67, `viewport.rs` 63, `effects.rs` 59, `inspector.rs` 58.

Per-file judgment on the top 10 (sampled diffs, not just counts):

- **Structural once landed, but the surface/vsync coupling area is a standing hazard**: `app.rs` /
  `app_render.rs` / `content_pipeline.rs` shared a 2-week burst (Apr 1–12) of 4+ *distinct* root
  causes under the same "hard lock" symptom (double-vsync deadlock, stale-display surface,
  drawable-pool reconfig block, shutdown circular-wait, fullscreen presenter block) — despite this
  repo already carrying standing memory warnings (`never-unify-cvdisplaylinks`,
  `direct-display-cadence`) about exactly this area. The warnings didn't stop new distinct bugs from
  landing in the same neighborhood.
- **`content_thread.rs`**: converged well — `f82d2859` explicitly "replace all timing-based fudge
  factors... with deterministic state convergence checks," a textbook structural fix, reached via a
  4-commit, 2-day iteration rather than first try.
- **`ui_root.rs`**: clearest stopgap→structural arc in the dataset — `556578c3` (Jul 7) is an
  explicit named stopgap, superseded next day by `6e4bddcb` landing DRAG_CAPTURE_DESIGN P1, which
  the backlog confirms closed BUG-058/059/075. Two more drag/widget-id patches landed same day
  after — the redesign surfaced adjacent bugs rather than closing the whole class in one shot.
- **`content_pipeline.rs`**: TexturePool was disabled outright after a dangling-pointer segfault
  (`5575bf04`, Mar 27, commit message: "heap textures cause GPU memory aliasing") and stayed off
  **~2.5 months** before being properly revived with instrumentation/eviction (`80e99f2b`, Jun 11).
  Reads as one fix in a naive commit count; is actually abandon-then-redo. Zero trace in
  BUG_BACKLOG.md (predates its 2026-06-23 start).
- **`viewport.rs`**: ~3 months of unrelated small UI-polish patches (tint bands, grid density, ruler
  clamp) with no shared root cause, until a scoped Jul 4 pass ("timeline P0.1: one Y source + one
  scroll owner") explicitly consolidated prior defects into one invariant — symptom-patched for
  months, then collapsed structurally in one pass.
- **`effects.rs`**: BUG-036's fix is well-diagnosed and tested, but BUG-080 (still open, "wants an
  Opus design pass") is the same file's acknowledged-unfixed structural root — a fixed symptom
  sitting on top of an admitted-open class.
- **`inspector.rs`**: same one-off pattern as viewport.rs, plus BUG-060 is REOPENED as of 2026-07-08
  — a live example of a fix that didn't close its class.

**Surprise not visible from BUG_BACKLOG.md alone**: the backlog only exists since 2026-06-23
(`ade06dbc`). The entire March–May history — the largest bursts in `app.rs`/`app_render.rs`, the
full Unity-port stabilization, the wgpu→native-Metal migration, the TexturePool abandon-and-retry —
has **zero backlog coverage**. Any structural/symptom judgment for that period rests on commit
messages and diffs alone, not on the same documentation discipline the post-06-23 corpus has.

## §5 Deltas vs FOUNDATIONAL_GAPS/CORE_ENGINE_FINDINGS (candidate new clusters)

1. **Freeze/fusion compiler correctness (BUG-006–012, 014)** — the single largest delta. No A-cluster
   or F-finding covers it; `FREEZE_COMPILER_MAP.md` exists as a map but names no enforcement gap of
   this shape. Candidate: a new cluster (call it A8) — "fusion-time invariants (state-statelessness,
   cycle-array construction, array-length agreement, single-entry-point, content-key hash stability)
   have zero enforcement despite an 8-bug campaign that found violations of all of them."
2. **BUG-079 + BUG-080 (silent fallback / two-phase param-manifest construction)** — both self-
   describe as structural and unfixed; BUG-080 explicitly asks for an Opus design pass. Neither maps
   to an existing A-cluster. BUG-079 (silent passthrough fallback, only an `eprintln!`) is also a
   direct instance of this repo's own hard rule "no-silent-fallbacks-or-interim-stopgaps" being
   violated in shipped code — worth flagging because the rule already exists and wasn't caught here.
3. **Cross-crate hot-path violations** — F7 (CORE_ENGINE_FINDINGS) only covers `manifold-playback`'s
   hot path (allocations + O(n²) scan in `sync_clips_to_time`). BUG-035 shows the same
   anti-pattern — a per-frame heavy conversion on the content thread — recurring in
   `manifold-app/content_pipeline.rs`, a sibling crate neither doc's hot-path lens currently reaches.
4. **BUG-069 (licensing)** sits fully outside both docs' taxonomy — not a patch-trail cluster, not a
   map-coverage gap, a dependency/legal risk. It's already tracked in agent memory
   (`audio-analysis-accuracy`) but not in either structural doc; flag as intentionally out-of-scope
   for a code-structure verdict rather than a missed cluster.
5. **Pre-backlog git history (§4)** — the TexturePool abandon-and-retry and the multi-cause vsync
   saga are real structural incidents with zero BUG_BACKLOG.md trace. Not a "cluster" in the
   A1–A7/F1–F17 sense, but a corpus-completeness gap worth naming for the verdict pass: verdicts
   drawn only from the backlog undercount manifold-app's/manifold-gpu's actual historical churn.

## §6 Open questions for the verdict pass

1. Should freeze/fusion-compiler (§5.1) become a formal FOUNDATIONAL_GAPS cluster (A8), given it's
   the largest uncovered delta and already has a live tracking doc (FREEZE_COMPILER_MAP.md) to anchor
   it to?
2. Is BUG-080 (param-manifest two-phase construction) the same underlying shape as F5
   (`clock_authority`/`bpm` mutated outside `EditingService`) — both are "an invariant holds except
   during a special-cased construction/mutation window" — worth a single unified verdict instead of
   two separate ones?
3. `manifold-playback` has both the worst backlog fix-ratio (1/8) and the entire open F1–F17 queue.
   Is this bandwidth (nobody's gotten to it) or does sync/timing complexity structurally resist the
   fix patterns that worked elsewhere (`manifold-io`'s guard+test pattern, `manifold-audio`'s
   accuracy-gate pattern)? The verdict pass should say which, since the fix shape differs either way.
4. Given §4's surprise (zero backlog coverage before 2026-06-23), should the verdict pass treat
   pre-backlog git history as first-class evidence, or explicitly scope verdicts to the
   backlog-covered period and flag the gap as a caveat?
5. BUG-069 (licensing) — does a "structural" verdict pass even have a bucket for it, or should it be
   explicitly excluded with a pointer to `audio-analysis-accuracy`?
6. `docs/BUG_BACKLOG.md`'s "Checked and safe" section shows the *correct* pattern for
   identity-on-duplicate (graph-node paste: fresh ids, remapped wires, 2 regression tests) sitting a
   few hundred lines from BUG-001–005, which lacked exactly that pattern. Is A5's fix simply "port the
   graph-node pattern to the other duplication paths," or is there a structural reason it wasn't
   already reused (e.g. `EffectId`/`ClipId` not sharing a trait/macro the way `NodeId` duplication
   does)? Worth a direct code comparison before the verdict pass assumes it's a copy-paste fix.
