# Render-Scene Perf Optimization — retiring BUG-189's ~10 ms import-graph GPU floor

**Status:** SHIPPED 2026-07-17 — all phases (P0–P4 + P3b) landed and P5's final re-measure is in
(Sonnet, orchestrated overnight per Peter's explicit mandate: "finish the optimisations end to end
in this orchestration session using sonnet agents") · design 2026-07-16 (Fable) · APPROVED
2026-07-16. **Final numbers (AMG GT3, M4 Max, two-consecutive-run pairs, `cargo xtask perf-soak`):**
@3840×2160 GPU p50 13.554ms → **9.45ms** (~4.1ms/~30% drop); @1920×1080 GPU p50 9.830ms → **5.73ms**
(~4.1ms/~42% drop). BUG-189's shadow+IBL re-render waste is closed; the residual is `render_scene`'s
main pass — real work, not staleness, and now essentially 100% of render_scene's own GPU time in
steady state (no separately-labeled shadow/IBL rows survive a profiled run) — R4 (indexed-mesh
rendering, deferred) is the next lever, per the Deferred section below. BUG-190 (BrainStem CPU cost)
was diagnosed, not fixed, per this doc's own D3/D3b scope — see `docs/BUG_BACKLOG.md`. This document
was the execution contract for an unattended Sonnet-orchestrated build; every decision was closed,
zero executor discretion. Scoping authority for anything this doc did not answer was Fable, not the
orchestrator — if a phase hit an undecided fork, the rule was STOP and surface it, never improvise
(Peter's instruction to the orchestrator, verbatim: "you do not have permission to make decisions
yourself unadvised").
**Prerequisites:** PERF_BUDGET_GATE_DESIGN.md P1+P2+P2b SHIPPED (`7afcb059`/`49f5a066`) — perf-soak
is this design's sole measurement oracle. GLTF_ANIMATION_DESIGN.md A1–A3 SHIPPED. Nothing here
waits on A4 or on SCENE_SETUP_PANEL_DESIGN.md (see D9).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.
**P3 amendment (2026-07-17, post-P3):** the mechanism landed exactly as specified and every
phase-local correctness gate passes (I2 animated-envmap parity, I4 static bit-identity, per-producer
gpu-proofs tests on `bake_equirect_envmap`/`hdri_source`/`render_scene`, all on real GPU hardware) —
but the phase's OWN perf gate (a multi-ms AMG @4K delta matching P0's measured ~41% IBL share) FAILS
on a real glTF import: measured p50 13.554ms → 13.333ms, ~0.22ms/1.6%, not multi-ms. Root cause is
outside this phase's file scope: every glTF import wires
`node.bake_environment → node.switch_texture (env_mode select) → node.render_scene`, never a direct
wire, and `node.switch_texture` copies its selected branch into its own output every frame without
ever declaring `mark_outputs_unchanged` — so `render_scene`'s envmap generation never stabilizes on
a real import and P3's `ibl_cache_key` misses every frame. Filed as BUG-197 (`docs/BUG_BACKLOG.md`),
which also updates BUG-189's fix-shape note. P3's code is safe and correct to keep (any DIRECTLY-
wired envmap — a hand-authored generator preset, or `switch_texture` once BUG-197 lands — gets the
full benefit today), but BUG-189's floor is NOT closed by P3 alone; do not treat this phase as having
delivered its headline number without BUG-197 landing first.

BUG-189: the glb import graph burns ~10 ms of true GPU time per frame *regardless of resolution*
(9.8 ms @1080p, 13.5 ms median / 22.7 ms p95 @4K, AMG GT3, 302k tris / 78 materials, M4 Max).
On stage that is well over half the 16.6 ms frame budget spent re-computing work whose inputs
have not changed since the previous frame: shadow maps re-render every frame for a light that
never moved, IBL convolution re-runs (~45M envmap samples) for an envmap that never changed, and
static mesh/texture decodes re-blit into their output slots every frame. What this buys on stage
when fixed: an imported glb scene costs (nearly) nothing while it stands still — the budget goes
to the camera moves, lens gestures, and effects Peter actually performs, and only re-costs when
something in the scene genuinely animates. Root cause of the whole class: the graph runtime has
no per-slot "unchanged this frame" signal, so every consumer must assume dirty always.
Companion: BUG-190 (BrainStem.glb, 24 skinned objects, flat ~370 ms/frame) gets its diagnosis —
not a guessed fix — in P0. Hardening level (DESIGN_DOC_STANDARD §9): conformance treatment —
anchors carry re-derivation commands.

## 1. Audit — what exists (verified 2026-07-16 against the live tree; re-derive at each phase)

| Piece | Where | State |
|---|---|---|
| Shadow maps re-render fully every frame, no dirty check | `render_scene.rs` ~2647–2708 (`shadow_caster_draws` rebuild + per-caster `draw_instanced_depth_only_batch`, encoder label `"node.render_scene shadow"`) — re-derive: `rg -n '"node.render_scene shadow"' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` | **waste (~4% measured — D1b; the headline moved to IBL)** |
| `shadow_view_proj()` is a pure function of the light alone | `node_graph/light.rs:333` | exists — static light ⇒ bit-identical map every frame |
| Importer wires shadows on, 4096² | `node_graph/gltf_import.rs:765` (`cast_shadows`=1.0), `:778` (`shadow_resolution`=4096.0) | exists |
| IBL re-convolves every frame envmap is wired | `run_ibl_convolution`, `render_scene.rs:1428`; its own doc comment (~1410–1427) states `bake_equirect_envmap.run()` rewrites its output in place every frame, so an identity-based skip would go stale — the exact hazard D5/D6 below resolve with a generation signal instead | **the headline waste (~41% measured — D1b), with a documented staleness trap** |
| Build-once precedent | `brdf_lut_built` (`render_scene.rs` ~537, ~1410) — LUT built exactly once per device | exists — the pattern to generalize |
| Identity-gating precedent | `gltf_texture_source.rs` `last_key` (~105, ~186) + `last_mip_identity` (~128, ~281): decode gated on param key, mip regen gated on output-texture identity — but the level-0 blit dispatch (~308–330) still runs every frame | exists, partial |
| Static sources re-copy every frame | `gltf_mesh_source.rs` (module doc: staging "re-fills the output buffer every frame via a cheap blit"), `gltf_skinned_mesh_source.rs` ~199–219 (three `copy_buffer_to_buffer` per frame), `gltf_texture_source.rs` blit above; sweep: `rg -n 'copy_buffer_to_buffer|dispatch_compute' crates/manifold-renderer/src/node_graph/primitives/gltf_*_source.rs` | **waste (R1)** |
| Non-indexed geometry | `flatten_primitive`, `gltf_load.rs:462` — indices expanded to flat triangle lists at import; measured 3.84× vertex amplification on the AMG (236,428 unique verts vs 907,476 index entries), paid in main pass AND every shadow pass | exists (R4 — DEFERRED, see D2) |
| CPU hot path | `render_scene.rs` `evaluate()` ~2264+ — ~22 `format!` allocations per object per frame (`rg -c 'format!' …/render_scene.rs` → 63 sites; the rebuild-time ones at ~705–743 are fine, the evaluate-time ones are not) + `bindings.rs:48–53` linear `iter().find` port scan ⇒ O(objects × wired_ports) ≈ O(objects²) | **waste (R5)** |
| Executor slot mechanics | `execution.rs:66` `Executor` (typed write scratches lines 70–85); `Slot(pub u32)` `bindings.rs:31`; no per-slot generation anywhere: `rg -n 'generation' crates/manifold-renderer/src/node_graph/execution.rs` → zero hits | generation signal **missing** (R2) |
| Measurement oracle | `cargo xtask perf-soak <glb> [--size WxH] [--frames N] [--profile]` — `manifold-app/src/perf_soak_import.rs`; unprofiled GPU p50/p95 = the honest absolute numbers; `--profile` = per-span attribution, shares-not-totals (D6 of the gate design) | exists |
| Attribution gap | `perf_soak_import.rs` `run_profiled` (~330–345): spans whose tag matches no executor step collapse into one `untagged_ms` scalar — but each span already carries its encoder pass label (`manifold-gpu/src/metal/profiling.rs` span `label` ~78/111; reserve sites `metal/encoder.rs` 265/299/327, labels like `"node.render_scene shadow"`, `"node.render_scene ibl prefilter"`, `"node.render_scene ibl irradiance"`) — the split R0 needs is recorded and then thrown away at report time | **missing (P0 fixes)** |
| Fixtures | `tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb` (BUG-189), `tests/fixtures/gltf/khronos/BrainStem.glb` (BUG-190) | exist |

Extend, don't redesign: every fix below is an existing in-repo pattern (`brdf_lut_built`,
`last_key`/`last_mip_identity`, `DataVersion` dirty-check discipline) generalized one step.

## 2. Decisions

- **D1 — Tonight's scope is R0 + R1 + R2 + R3 + R5; R4 and R6 are deferred.** R0 (bisection +
  BrainStem diagnosis) is measurement and gates everything else's before/after claims. R1 (static-
  source emission gating) is small, has an in-file precedent, and is R2's trust prerequisite. R2
  (per-slot write generations + dirty-gated shadow caching) is the headline win — on a static
  scene it eliminates the shadow share of the floor after frame 1, stays free under live camera/
  lens moves (the light is untouched by the camera; `shadow_view_proj` is light-only), and re-costs
  only when something actually animates. R3 (IBL gating) rides the same generation signal. R5
  (CPU `evaluate()` repair) is low-risk and becomes load-bearing the day SCENE_SETUP_PANEL P4's
  merge-import ships. Rejected: including R4 (indexed rendering) tonight — it is a design-doc-sized
  change (new `Array` index port semantics, `draw_indexed` in `manifold-gpu`, and a reconciliation
  pass over every mesh-consuming atom that assumes flat-triangle-corner layout — several deform
  atoms do); running that unattended, without Fable review of the port-type decision, is exactly
  the improvised-executor failure this doc exists to prevent. Rejected: "safe ones only" (R0/R1/R5,
  deferring R2/R3) — the headline waste IS the shadow+IBL re-render; shipping only the periphery
  would spend the night without touching BUG-189's actual floor.
- **D1b — amendment (2026-07-17, post-P0): D1's "headline win" attribution was wrong; the measured
  split reverses shadow and IBL.** P0's bisection (two consecutive profiled runs per resolution,
  rank order stable): render_scene's own GPU time splits ~54% main pass / ~41% IBL (prefilter +
  irradiance) / ~4% shadow, at BOTH 4K and 1080p (unprofiled anchors: @4K p50 13.554 ms p95
  13.869 ms; @1080p p50 9.830 ms p95 11.768 ms). So R3 (IBL gating, P3) is the payoff phase; R2's
  shadow caching reclaims only ~4% on its own. Build order is UNCHANGED: P2 remains P3's structural
  prerequisite — the per-slot generation signal lives in P2, and shadow caching is that signal's
  first proven consumer (the I1 mutation trio is what earns trust before IBL leans on it); splitting
  the infra out of P2 buys nothing on a serial night (D10). What changes: P2's perf gate is
  corrected (a ~4% delta ≈ ~0.5 ms @4K is inside unprofiled run-to-run noise — see the P2 gate
  text), P3's gate carries the measured anchor, and §Deferred's R4 entry is updated — main pass is
  the single largest share and cannot be dirty-gated (the camera animates on stage every frame), so
  R4's revival is now near-certain rather than speculative; its trigger (P5's measured residual)
  and the supervised-session requirement both stand.
- **D2 — R4 (indexed geometry) and R6 (GPU culling) are deferred with named triggers.** R4:
  revive as its own Fable/Opus design session once P0–P5 land and the re-measure (P5) shows the
  remaining main-pass vertex share still matters at 4K60 — it kills the 3.84× amplification
  everywhere including future A4 `skin_mesh` cost, so it is the highest-ceiling item, but it is
  supervised-session work. R6: revive when multi-GLB merged scenes are routine (post
  SCENE_SETUP_PANEL P4) and P5's numbers show main-pass draw cost dominating; needs AABB infra
  that does not exist in the graph today.
- **D3 — BUG-190/BrainStem is in scope as DIAGNOSIS ONLY, inside P0.** The 370 ms is not linear
  from single-object cost — building a fix against a guess is forbidden (the corpus rule that
  derivation isn't observation). P0 runs perf-soak (unprofiled + profiled) on BrainStem, separates
  GPU time from CPU encode wall time, and updates BUG-190's backlog entry with the measured root
  cause. If the root cause turns out to BE one of R1/R2/R3/R5's targets, the corresponding phase
  fixes it and P5 re-measures BrainStem to confirm; if it is something else, the backlog entry
  names it and it becomes its own follow-up — no improvised fix tonight. Rejected: a dedicated
  BrainStem fix phase tonight (fix shape unknown until P0 runs; an unattended session must not
  invent one).
- **D3b — amendment (2026-07-17, post-P0): BUG-190 diagnosis outcome + the animated-fixture
  measurement gap.** Measured: the originally-filed ~370 ms/frame does NOT reproduce on the current
  tip (original repro harness re-run, ~30 ms max — vanished regression, no bisect tonight; chasing
  a fixed bug is not the mandate). The residual is CPU-side: GPU p50 6.8 ms / p95 8.85 ms (healthy)
  vs CPU-encode-wall p50 21.4 ms / p95 22.4 ms (~3× the GPU side, 24 objects). Root cause of the
  residual is unattributed, but its shape is exactly R5's target — per D3's own rule, P4 is the
  test: BrainStem before/after CPU wall is P4's sensitive-fixture gate; if the wall is still
  >16.6 ms after P4, the backlog entry names a follow-up with numbers, no improvised fix. D3's
  diagnosis-only obligation is SATISFIED. Tool gap found en route: perf-soak's convergence-gated
  warmup never converges on a continuously-animated fixture, so BrainStem's diagnosis needed an
  uncommitted direct measurement — P4 gains a deliverable (a fixed-warmup override on import mode)
  so its BrainStem numbers come from committed, reproducible tooling.
- **D4 — R0 needs NO ablation flags and NO fixture surgery; the one tool change is report-side:
  unmatched profiled spans are grouped by their own label instead of collapsing into one scalar.**
  (This is an amendment to PERF_BUDGET_GATE_DESIGN.md D7/P2b — P0 appends it there as "D8 —
  unmatched-span label breakdown (import mode)".) The sampler already records one span per encoder
  pass with the pass's own label; `render_scene`'s internal passes are distinctly labeled
  (`… shadow`, `… ibl prefilter`, `… ibl irradiance`, `… shaft …`); only the report throws the
  labels away (`perf_soak_import.rs` ~342: `None => frame.untagged_ms += span.millis`). Fix: group
  unmatched spans by `span.tag` into rows `{tag: <label>, type_id: "unmatched", gpu_ms, share_of_frame}`
  alongside the matched node rows; delete the scalar `untagged` row (import mode only — project
  mode's "compositor/untagged" row was a D6 decision of that design; do not reopen it unattended).
  Shadow share = Σ rows whose tag starts `node.render_scene shadow`; IBL share = `… ibl` rows;
  main pass = the remainder of render_scene's spans. Honesty note carried from that design's D6:
  profiled totals are inflated (encoder splitting) — R0 reports the split as SHARES, anchored to
  the unprofiled p50 as the absolute number. Rejected: a `--set node.param=value` ablation flag
  (e.g. `--set sun.cast_shadows=0`) — it measures a configuration nobody ships, can't ablate IBL
  at all (the envmap is a required input for pbr materials — unwiring it hits the magenta-fallback
  error path; shrinking `envmap.width` doesn't reduce convolution sample count, which is fixed per
  output texel), and the per-label breakdown answers the question without any of that. Rejected:
  hand-editing the fixture or `gltf_import.rs` per ablation run — the import graph is assembled in
  code, so "hand-edit the .glb" was never real; rebuilding product source per measurement leg is
  unreproducible.
- **D4b — amendment (2026-07-17, mid-P0): D4's `None`-arm fix was correct but aimed at the wrong
  collapse site.** P0's executor found, by running the tool for real: `render_scene`'s internal
  GPU passes (shadow, IBL prefilter, IBL irradiance, main) never call `set_profile_tag` — they all
  share ONE tag (the node's), but each `reserve()` call site carries a distinct `label` (e.g.
  `"node.render_scene shadow"`, `"… ibl prefiltered specular"`). `perf_soak_import.rs`'s existing
  join groups by `tag` alone and never reads `span.label`, so these spans all land in the SAME
  matched (`Some`) row — never in the `None`/unmatched arm D4 addressed — collapsing to one row at
  `share_of_frame: 1.045`. Fix, corrected: **rows stay keyed by `tag` (one row per node, unchanged);
  each row gains a nested `passes` map keyed by `span.label`**, accumulating GPU ms per pass —
  NOT a flat `(tag, label)` row scheme (rejected: `label` alone would merge same-labeled passes
  across multiple instances of the same node type in one graph; a flat combined key would smear or
  duplicate the per-NODE-only `cpu_us` datum, which has no per-pass equivalent — `StepProfile`'s
  CPU join is per node, confirmed by reading `execution.rs`). `type_id` stays node-level, appearing
  once per tag row, never repeated per pass (a pass is not a node). Report shape: node row keeps
  its five existing fields (`tag`, `type_id`, `gpu_ms` = sum of its passes, `cpu_us`, `share_of_frame`)
  plus a new `"passes"` array — one `{label, gpu_ms, share_of_frame}` per distinct label under that
  tag, sorted descending by `gpu_ms`, always emitted (even length 1, no conditional shape). The
  `None` arm's key is corrected from `span.tag` to `span.label` to match its own comment's intent
  (a genuinely unmatched/untagged span groups by its own label — this was a latent bug in the
  just-written D4 code, not yet committed). No existing consumer breaks: PERF_BUDGET_GATE_DESIGN's
  I4 means profiled JSON never gates or writes a baseline; BUG-189's backlog table is a hand
  transcription of one past run, not a parser — do not rewrite it retroactively; `rg` over
  `scripts/`+`docs/` found no machine consumer of the `"nodes"` array. Positive gate, corrected: the
  `node.render_scene` row's `passes` array contains ≥3 distinct nonzero entries including one
  labeled `shadow` and one labeled `ibl`, with pass rank order stable across two consecutive runs —
  do NOT gate on shares summing to ≤1.0 (the observed 104% is D6's declared stage-boundary-sampling
  inflation plus denominator skew; P2/P3's before/after claims use pass-level ms and rank, never
  raw share-sum). Folded into P0, same phase, same file — not a re-scope.
- **D5 — The dirty signal is per-slot write generations with a conservative default: the executor
  bumps every output slot's generation for every node that runs, UNLESS the node explicitly
  declares "outputs unchanged this frame".** A `u64` per slot, owned by the `Executor` alongside
  its slot tables; exposed read-side as `ctx.inputs.slot_generation(port) -> Option<u64>`;
  declared write-side by a new `ctx.mark_outputs_unchanged()` (exact home: the node run context —
  P2 anchors it). Safety shape: a node that never calls the API keeps bumping ⇒ downstream caches
  keep missing ⇒ today's behavior, provably never stale. Staleness would require a node to
  *falsely* declare unchanged — a locally auditable, per-node property, enforced by parity tests
  on each declaring node. Only the R1-gated static sources (and R3's envmap bake) declare it
  tonight. A4 composes for free: an animating pose/skin path never declares unchanged, so
  generations bump and every downstream cache honestly re-costs — degradation is real work, not
  staleness. Rejected: content-hashing slot payloads (per-frame cost proportional to the data —
  the waste class this design removes); pointer/size identity keys on the consumer side (the
  exact trap `run_ibl_convolution`'s doc comment documents — in-place rewrites defeat identity);
  a global frame-dirty bit (one animating node would un-gate the whole scene).
- **D6 — Shadow caching is keyed on everything the shadow pass reads, plus a rebuild epoch;
  cache hit = skip the depth-only batch, cache miss = re-render that caster's map and store the
  key.** Per caster: key = hash of (`shadow_view_proj()` matrix bytes, shadow resolution, ordered
  per-caster-draw list of (model matrix bytes, vertices-slot generation, instances-slot generation
  or none, vertex count, instance count), draw-list length, executor rebuild epoch). The maps
  themselves already persist (`self.shadow_maps[slot]`) — caching is *skipping the re-render into
  them*, no new textures. The epoch term (a counter bumped on every executor rebuild, or
  equivalently cache-clear on rebuild — P2 verifies node-state lifetime across rebuild and picks
  the one the lifetime makes correct) prevents a false hit after generations reset. Rejected:
  keying on light params only (misses geometry/transform changes); keying on slot generations only
  (misses model-matrix changes that arrive as computed values rather than slot writes); hashing
  vertex buffer *contents* (that's the per-frame cost we're deleting — generations stand in for
  content).
- **D7 — IBL gating: `bake_equirect_envmap` first stops rewriting unchanged output (R1's
  `last_key` pattern: skip `run()`'s dispatch when (params, output-texture identity) unchanged,
  and declare outputs-unchanged per D5), THEN `render_scene` re-convolves only when the envmap
  slot's generation changed (or first use / envmap texture identity changed).** This resolves the
  staleness hazard `run_ibl_convolution`'s doc comment (~1410–1427) names — an animated envmap
  (the sun-coherence gesture) changes params ⇒ the bake really runs ⇒ generation bumps ⇒
  re-convolution happens. Order is load-bearing: gating consumption before the producer stops
  in-place rewrites would serve stale mips — hence R3 lands as one phase, producer first, with the
  parity test proving the animated case. `hdri_source` (file-path envmaps) gets the same audit in
  the phase: verify whether it re-blits per frame and apply the identical pattern if so.
- **D8 — Every phase's perf claim comes from unprofiled perf-soak runs on the AMG fixture,
  recorded in the phase commit message; profiled runs are diagnosis only.** Direct inheritance of
  PERF_BUDGET_GATE D6. Before/after = same machine, same `--size`, same `--frames`, back-to-back.
  Import mode is report-only by design (no baselines) — the phase gate is the delta between the
  two runs the phase itself performs, not a checked-in threshold. Rejected: adding an import-mode
  baseline tonight (reopens that design's D7 "report-only" decision unattended).
- **D9 — Dependencies: every phase is fully independent of GLTF_ANIMATION A4 and of
  SCENE_SETUP_PANEL P1–P5. No phase waits on any external landing.** A4 (not yet built): D5's
  conservative default means A4's future animated paths simply never declare unchanged — R2/R3
  compose with A4 without knowing its shape; nothing here keys on skinned-output identity in a way
  A4 could invalidate. A1–A3 (shipped): the animated pose flows through `gltf_skeleton_pose` /
  skinning atoms, NOT through the static rest-pose copies R1 gates — P1's brief still proves this
  with an animated-fixture parity test rather than trusting this sentence. SCENE_SETUP_PANEL
  (building concurrently overnight, other session): file-overlap audit — that work lives in UI
  dock/panel code and (P4) import assembly; tonight's phases touch `render_scene.rs`, four
  `gltf_*_source.rs` primitives, `bake_equirect_envmap.rs`, `execution.rs`/`bindings.rs`,
  `perf_soak_import.rs`, and docs — `gltf_import.rs` is READ, never edited, by every phase here.
  Honest sequencing note (not a blocker): R5's payoff is *multiplied* by merge-import landing, but
  building R5 first is strictly better — there is no benefit to waiting. The one real interaction
  is the shared main checkout at landing time: normal fetch/merge/gate/push loop per
  `.claude/GIT_TREE_DISCIPLINE.md`, batch per 2–3 phases.
- **D10 — Phases land serially in one worktree, one workstream slot.** P1–P4 all touch
  `render_scene.rs` or its immediate collaborators; parallel executors would conflict in-file.
  `python3 scripts/agent-worktree.py acquire render-scene-perf feat/render-scene-perf` once, all
  phases in it, release at end. Rejected: a slot per phase (the 2026-07-15 incident class; also
  pointless — the phases are sequential by data dependency R1→R2→R3).

## 3. Invariants & enforcement

- **I1 — Never serve a stale shadow map.** A cached map is served only while D6's full key
  matches; any input the shadow pass reads is in the key. Enforcement: P2's mutation parity tests
  (light param, object transform, mesh content each force a re-render whose output equals a
  fresh executor's) + the conservative-default rule (non-participating producers always bump).
- **I2 — Never serve stale IBL.** Same shape: P3's animated-envmap parity test (param change ⇒
  bake runs ⇒ re-convolution ⇒ output equals fresh render).
- **I3 — `mark_outputs_unchanged` is truthful per node.** A node may declare it only when its
  outputs are bit-identical to the previous frame *including physical output identity*.
  Enforcement: per-declaring-node gpu-proofs parity tests (P1/P3); the API doc comment states the
  contract; review checklist in each brief.
- **I4 — Caching changes are invisible at the pixel level.** Frame N of a static scene is
  bit-identical to frame 1 of a fresh executor (readback compare). Enforcement: P2/P3 positive
  gates.
- **I5 — Perf claims come from unprofiled runs; profiled runs never produce a phase's headline
  number.** Enforcement: D8; phase gates name the unprofiled command explicitly.
- **I6 — No new `Arc<Mutex>`/shared state.** Generations live in the `Executor`, single-threaded
  with the graph walk. Enforcement: review + `rg -n 'Arc<(Mutex|RwLock)' crates/manifold-renderer/src/node_graph/` unchanged.

## §. Phasing

Worktree: one slot for the whole workstream (D10). Every brief opens with the base-verification
guard (`git log --oneline -1` matches the intended tip). Perf-run commands assume the fixture
paths verified in §1.

**P0 — measurement: span-label breakdown + BUG-189 bisection + BUG-190 diagnosis (half session, Sonnet).**
Entry: PERF_BUDGET_GATE P2b shipped (verify: `cargo xtask perf-soak tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb --size 3840x2160 --frames 100` runs and emits `"mode": "import"` JSON).
Read-back: this doc §1–§2 (D4 especially); PERF_BUDGET_GATE_DESIGN.md D6+D7; `perf_soak_import.rs`
`run_profiled` end-to-end; `manifold-gpu/src/metal/profiling.rs` module doc.
Deliverables: (1) in `run_profiled`, unmatched spans grouped by `span.tag` into per-label rows
(`type_id: "unmatched"`) replacing the `untagged_ms` scalar — import mode only, project mode
untouched; (2) the amendment note appended to PERF_BUDGET_GATE_DESIGN.md's Status header + a "D8"
decision entry there, per D4's exact text; (3) bisection runs on the AMG at `--size 3840x2160` and
`--size 1920x1080`: one unprofiled (absolute anchor) + two consecutive profiled runs (rank-order
stability check, per that design's P2b gate) — record shadow / IBL / main-pass / other SHARES and
the unprofiled p50/p95 in the commit message and in BUG-189's backlog entry; (4) the same pair of
runs on `tests/fixtures/gltf/khronos/BrainStem.glb`, plus the GPU-vs-CPU-encode-wall split from the
stats JSON — update BUG-190's backlog entry with the measured root cause (or "still unknown" +
what the numbers exclude); diagnosis only, no fix (D3).
Gate — positive: on the AMG profiled run, rows labeled `node.render_scene shadow`, `… ibl …`
appear with nonzero shares and the top-5 rank order is stable across the two consecutive runs;
negative: `rg -n 'untagged_ms' crates/manifold-app/src/perf_soak_import.rs` → zero hits (the
scalar collapse is gone); `rg -n 'update_baseline' crates/manifold-app/src/perf_soak_import.rs`
still shows no baseline write reachable from import mode.
Demo: the attribution JSON for the AMG's worst frame + the four share numbers, read by the
orchestrator — L2. Forbidden moves: any ablation flag (`--set`, `--drop` — D4 rejected them); any
change to project-mode attribution or its "compositor/untagged" row; any change to unprofiled
measurement; building any BrainStem fix; treating profiled TOTALS as absolute numbers anywhere in
the recorded findings (shares only, anchored to unprofiled p50).
Test scope: focused (`-p manifold-app --lib` if the touched code has tests; the tool run itself is
the real gate). Dependencies (D9): none — fully independent, start immediately.

**P1 — R1: static-source emission gating + the unchanged-declaration API stub (one session, Sonnet).**
Entry: P0 landed (its share numbers are this phase's before-anchor).
Read-back: this doc D5 + I3; `gltf_texture_source.rs` whole file (the `last_key`/`last_mip_identity`
pattern IS the template — ~105/128/186/281 and the blit at ~308–330); `gltf_mesh_source.rs` and
`gltf_skinned_mesh_source.rs` whole files; `docs/EFFECT_CHAIN_LIFECYCLE.md` (state-cache eviction);
sweep for further per-frame copies: `rg -n 'copy_buffer_to_buffer|dispatch_compute' crates/manifold-renderer/src/node_graph/primitives/gltf_*_source.rs`.
Deliverables: (1) `gltf_texture_source`: skip the level-0 blit dispatch AND mip regen when
(decoded-content key unchanged AND output-texture identity unchanged) — extending the existing
`last_mip_identity` discipline to the blit; (2) `gltf_mesh_source` + `gltf_skinned_mesh_source`
(+ `gltf_morph_deltas_source` if the sweep shows the same pattern): skip the staging→output copies
when (cached content unchanged AND same physical dst identity as last frame); (3) the
`ctx.mark_outputs_unchanged()` API on the node run context, stored by the executor per node per
frame, READ BY NOTHING yet (P2 consumes it) — each gated source calls it exactly on its skip path;
(4) doc comment on the API stating I3's contract verbatim.
Gate — positive: gpu-proofs parity per touched source — frame 2's output readback bit-identical
to frame 1's on a static asset; a param/content change (e.g. `mode` flip on texture source, path
change on mesh source) produces output equal to a fresh executor's; on a skinned+animated fixture
(`BrainStem.glb` or any khronos animated asset), the animated pose path is unaffected (readback of
an animated frame differs from frame 1 exactly as it does pre-change) — this is D9's "prove it,
don't trust the sentence" test; unprofiled perf-soak before/after on the AMG recorded in the
commit message (expected: small GPU delta, nonzero — blit/copy cost — plus reduced blit rows in a
profiled sanity run); negative: `rg -n 'mark_outputs_unchanged' crates/manifold-renderer/` shows
calls ONLY in the gated sources' skip paths and the executor storage — no consumer yet.
Demo: before/after perf-soak JSON — L2. Forbidden moves: gating on pointer identity of the OUTPUT
without also keying content (pool recycling hands back different physical textures — the
`last_mip_identity` precedent exists precisely for this); declaring unchanged anywhere except the
exact skip path (I3); touching `render_scene.rs` (that's P2/P3); a generation counter (P2 owns it).
Test scope: `cargo test -p manifold-renderer --features gpu-proofs <touched_module>::gpu_tests`
per source + default sweep (`cargo nextest run --workspace`) before commit. Dependencies (D9):
none external; requires P0's numbers only as the before-anchor.

**P2 — R2: per-slot write generations + dirty-gated shadow caching (one session, Sonnet — P3's prerequisite infra; see D1b).**
Entry: P1 landed (sources declare unchanged; the declaration is stored).
Read-back: this doc D5+D6+I1+I4; `execution.rs` end-to-end (write scratches 70–85, slot binding,
node walk — find every point where a node's outputs are committed); `bindings.rs` (Slot,
NodeInputs); `render_scene.rs` shadow section ~2647–2708 + `light.rs:333`; verify node-state
lifetime across executor rebuild before choosing D6's epoch-vs-clear variant (oracle: read the
rebuild path in `execution.rs`, don't infer).
Deliverables: (1) `Vec<u64>` slot generations in `Executor`, bumped per D5's conservative rule at
the single choke point(s) where a node's outputs are committed — every output slot of every
executed node bumps unless that node declared unchanged this frame; (2) read-side
`ctx.inputs.slot_generation(port)`; (3) shadow-map caching in `render_scene`: per-caster key
exactly as D6 specifies (vp bytes, resolution, ordered per-draw (model bytes, vertices-slot gen,
instances-slot gen, vcount, instance_count), draw-list length, rebuild epoch) — on full-key match
skip that caster's `draw_instanced_depth_only_batch`; any mismatch → re-render + store key;
(4) the key computation allocates nothing per frame (hash into a fixed hasher; pre-allocated
scratch — hot-path discipline).
Gate — positive: (a) correctness: gpu-proofs tests proving I1 — static scene: frame 30 color
output bit-identical to fresh-executor frame 1 (I4); mutation trio: change a light param → next
frame equals a fresh render; change an object transform → same; change mesh content (source param)
→ same; (b) perf: PRIMARY evidence is pass-level and profiled (D1b: shadow is only ~4% of
render_scene GPU time — ~0.5 ms @4K, inside unprofiled run-to-run noise, so an unprofiled p50
delta cannot gate this phase; D4b licenses pass-level ms/rank for exactly this): steady-state
profiled runs show the `node.render_scene` row's `shadow` pass at <0.1 ms and <1% share (P0
measured ~4%); unprofiled perf-soak before/after on the AMG @4K is still run and recorded in the
commit message as direction sanity, NOT a pass/fail threshold; negative: `rg -n 'Arc<(Mutex|RwLock)' crates/manifold-renderer/src/node_graph/`
unchanged (I6); the cache key function provably includes the rebuild epoch
(`rg -n 'epoch' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`).
Demo: before/after unprofiled JSON + the three mutation tests green — L2. Forbidden moves:
serving a cached map on ANY key-component mismatch (never serve a stale shadow map — I1 verbatim);
skipping the epoch/rebuild-invalidation term; content-hashing vertex buffers (D6 rejected);
bumping generations anywhere except the committed-output choke point (scattered bump sites = a
missed one = staleness); making `--profile` numbers the phase's headline (I5); gating anything in
render_scene other than the shadow pass (IBL is P3's, with its producer-first ordering).
Test scope: `cargo test -p manifold-renderer --features gpu-proofs` (render_scene gpu_tests +
the new tests) + default workspace sweep; this touches the graph runtime, so the GPU feature run
is mandatory, on `cargo test`, never nextest. Dependencies (D9): P1 only. Fully independent of
A4/SCENE_SETUP_PANEL.

**P3 — R3: IBL convolution gating, producer first (half session, Sonnet).**
Entry: P2 landed (generation signal live and shadow-proven).
Read-back: this doc D7+I2; `bake_equirect_envmap.rs` whole file (params ~59–150, `output_dims`
~218); `run_ibl_convolution` + its doc comment, `render_scene.rs` ~1410–1527; `hdri_source.rs`
(the per-frame audit D7 names).
Deliverables: (1) `bake_equirect_envmap`: skip `run()`'s dispatch when (full param key, output
identity) unchanged — `last_key` pattern — and call `mark_outputs_unchanged()` on the skip path;
(2) `hdri_source`: same audit; apply the identical gate if it re-emits per frame, or record in the
commit message that it already doesn't; (3) `render_scene`: run `run_ibl_convolution` only when
the envmap slot's generation changed since last convolution (or first frame / envmap identity
changed); update the ~1410 doc comment — it currently documents WHY the skip is unsafe; after this
phase it must document why the generation signal makes it safe (supersession discipline, in-file);
(4) delete or update the `ESCALATION_FP1.md` open-question reference if that file's question is
now answered (rg it; tombstone, don't leave the stale claim).
Gate — positive: gpu-proofs animated-envmap parity (the I2 test): change an envmap param (e.g.
`horizon_strength`) → next frame's lit output equals a fresh executor's render; static-envmap
bit-identity across frames (I4 extension); unprofiled perf-soak before/after on the AMG @4K
recorded, delta consistent with P0's measured IBL share — ~41% of render_scene GPU time (D1b), a
multi-ms delta well above run noise (±30% tolerance stands); profiled sanity: `… ibl prefilter` /
`… ibl irradiance` rows <2% on steady frames; negative: `rg -n 'go stale' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`
→ the old warning text is gone/rewritten.
Demo: before/after JSON + the animated-envmap test green — L2. Forbidden moves: gating
consumption before the producer stops in-place rewrites (D7's load-bearing order — within the
phase, the bake gate lands and is tested before the render_scene gate is enabled); identity-only
keys (the documented trap); touching the BRDF LUT path (already build-once, correct).
Test scope: same as P2 (GPU feature run mandatory — shaders' callers changed). Dependencies (D9):
P2 only.

**Perf clause discharged by P3b** (2026-07-17, post-P3): the full-chain AMG before/after (bake
→ switch_texture → render_scene) is owed by P3b, not this phase — BUG-197 found every glTF import
routes through `node.switch_texture`, which broke the generation signal this phase's mechanism
correctly relies on. This phase's own mechanism and correctness gates are unaffected and remain
landed as-is.

**P3b — BUG-197: pass-through generation gating on `node.switch_texture` (half session, Sonnet).**
Entry: P3 landed with its perf clause undischarged (P3's own gate note) — the mechanism is correct
but every glTF import wires `bake_environment → switch_texture → render_scene`, and the mux
re-emits every frame, so render_scene's IBL cache key never hits on a real import (BUG-197).
Read-back: this doc D5+D7+I1–I4 and the P3 brief; `mux_texture.rs` WHOLE file — especially the
`latched_selector` doc (~119–132: wired selectors render with LAST frame's value by design; the
gate keys on the effective value, so this is already solved, don't re-derive it),
`selected_input_branch`, `skip_passthrough`, and `is_pure`; `execution.rs` alias-skip path
(~1093–1160) + the D5 choke-point comment (~1296–1319) — it documents WHY aliased steps
conservatively bump; after this phase it must document why the fenced propagation is safe
(supersession discipline, in-file); `gltf_import.rs` ~687–718 (the D6 env wiring: selector is
INLINE, `num_inputs = 2`); P2's `slot_generation` read-side API.
Deliverables: (1) `mux_texture.rs` evaluate-path gate: after the existing selector-read/latch
block (which must run unconditionally — the latch update is the wired-selector contract), compute
key = (effective selector index actually rendered, selected source slot's `slot_generation`,
selected source texture identity, output texture identity, rebuild epoch); on full-key match skip
the dispatch (and the clear-fallback) and call `mark_outputs_unchanged()`; any mismatch → run +
store key. The unwired-fallback (`in_0`) and all-unwired (clear-to-black) paths participate in the
key like any other resolved source — same rule, no special cases. (2) `execution.rs` alias-path
propagation, fenced: for a `performed_alias` step where `data_skip` is FALSE (param-driven
`skip_passthrough` only — the empty-propagation data-skip path keeps its conservative bump
untouched), set `node_declared_unchanged[idx]` when (same in-slot resource as this step's previous
frame, same out slot, in-slot generation unchanged); per-step scratch in `Executor`, cleared on
rebuild. (3) Update the choke-point comment and BUG-197's Status line; note in the commit message
whether the AMG's mux took the evaluate path or the alias path (expected: evaluate — equirect
dims ≠ canvas dims, so `compatible()` fails). (4) One recorded observation, no code: with
`env_mode = HDRI` the chain routes through `node.exposure` (hdri_gain), which never declares
unchanged — run the AMG once in HDRI mode after landing and record whether the floor drops there
too; if not, extend BUG-189's residual note naming the exposure hop (fix is future R-class work,
NOT this phase).
Gate — positive: (a) full-chain perf, transferred verbatim from P3's undischarged clause:
unprofiled perf-soak before/after on the AMG @4K (default import = softbox path), delta consistent
with P0's measured IBL share — ~41% of render_scene GPU time, a multi-ms drop well above run
noise (±30% tolerance stands); profiled sanity: `… ibl prefilter` / `… ibl irradiance` rows <2%
on steady frames AND the `node.switch_texture` dispatch row gone from steady frames; (b)
correctness, both selector populations: gpu-proofs — static inline selector + static source:
frame N output bit-identical to fresh-executor frame 1 (I4 through the mux); static selector +
CHANGED source (flip a bake param): next frame's lit output equals a fresh executor's (I2 through
the mux — proves the generation term); CHANGING selector, inline: flip `selector` → output equals
a fresh render with the new branch, next frame; CHANGING selector, WIRED: drive the selector wire
across a change and assert output matches the LATCHED expectation (frame N+1 shows the new
branch, exactly as pre-change — the latch is existing behavior, the test pins that the gate didn't
break it and introduces no staleness beyond the designed one-frame lag); alias-path test: a
dims-matched mux chain (canvas-sized source) proving the executor propagation — static input →
downstream consumer's generation stable; input re-emits → generation bumps; negative:
`rg -n 'node_declared_unchanged' crates/manifold-renderer/src/node_graph/execution.rs` shows the
alias-path write is inside a `!data_skip` guard; the P3 brief's gate paragraph carries a one-line
note "perf clause discharged by P3b" (this doc, in-file).
Demo: before/after JSON + the wired-selector staleness test green — L2. Forbidden moves: touching
the latch semantics or `selected_input_branch` pruning (the one-frame selector lag is designed,
documented, and consumed by liveness — changing it is a different design); declaring unchanged on
the `data_skip` alias path (empty-propagation chains keep the conservative bump); gating
`node.exposure` or any other atom (recorded observation only — scope is the mux + the alias choke
point); identity-only keys (P1's documented trap — the generation term is the point); removing
the mux's `is_pure`/`skip_passthrough` declarations (they're orthogonal optimizations, not
casualties).
Test scope: `cargo test -p manifold-renderer --features gpu-proofs` (mux gpu_tests + render_scene
suite + the new full-chain tests) + default workspace sweep; graph runtime touched → GPU feature
run mandatory, on `cargo test`, never nextest. Dependencies (D9): P3 only. P4 remains
data-independent (its entry clause is unchanged); D10 serial ordering in the same worktree still
applies.

**P4 — R5: CPU evaluate() repair (half–one session, Sonnet).**
Entry: P3 landed (or P2 landed and P3 blocked-and-surfaced — P4 is data-independent of P3).
Read-back: this doc §1's CPU row; `render_scene.rs` `evaluate()` region ~2264 onward and the
rebuild-time name generation ~705–743 (names are ALREADY format!-generated once at rebuild — the
evaluate-time re-formatting is pure waste); `bindings.rs:40–55`; hot-path discipline (CLAUDE.md).
Deliverables: (1) per-object port-index tables built once at `rebuild()` (object index → the
resolved binding indices for mesh/material/maps/transform ports), replacing every per-frame
`format!` + `iter().find` in `evaluate()` — lookups become direct indexing; (2) same treatment for
the per-light and any other per-frame formatted lookups in the same path; (3) zero per-frame
allocations in the repaired region (pre-allocated scratch where a collection is unavoidable);
(4) perf-soak import mode: a fixed-warmup override flag (e.g. `--warmup-frames N`) that bypasses
the convergence gate — report-only, no baseline interaction, project mode untouched — required
because a continuously-animated fixture never converges (D3b); this phase's BrainStem numbers use
it, with the flag value recorded in the commit message.
Gate — positive: output unchanged — the existing render_scene gpu-proofs suite green, plus one
readback bit-identity check before/after on the AMG frame 1; perf: perf-soak CPU encode wall time
(the stats JSON reports it) on BrainStem AND the AMG, before/after in the commit message —
BrainStem is the sensitive fixture (24 objects; if P0 measured the CPU side as material, this is
where it shows); negative: `rg -n 'format!' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`
→ remaining hits are rebuild-time or error paths only (annotate the gate output with the
classification); `rg -n 'iter\(\)\s*\.find' crates/manifold-renderer/src/node_graph/bindings.rs`
→ the hot lookup no longer routes through it (either an indexed accessor beside it, or the scan
kept solely for cold/error paths and documented as such).
Demo: before/after CPU wall numbers — L2. Forbidden moves: changing any port NAME or binding
semantics (this is a lookup-mechanics change only); "fixing" BUG-190 beyond what this repair
delivers (D3 — if BrainStem is still slow after, the backlog entry says so with numbers); static
name tables (object count is unbounded — the ~2266 comment already rejects them; tables are built
per-rebuild, sized to the instance).
Test scope: default sweep + `cargo test -p manifold-renderer --features gpu-proofs` (render_scene
suite — its callers changed). Dependencies (D9): P1's landing only (file adjacency in
render_scene.rs makes serial ordering mandatory per D10, but there is no data dependency on P2/P3).

**P5 — re-measure, backlog truth, supersession sweep (half session, Sonnet).**
Entry: P1–P4 landed.
Deliverables: (1) full re-measure: unprofiled + profiled perf-soak on the AMG @1080p and @4K and
on BrainStem, numbers into BUG-189's and BUG-190's backlog Status lines (BUG-189: fixed/residual
with the new floor; BUG-190: fixed / re-diagnosed with P0+P4 evidence); (2) supersession sweep per
CLAUDE.md: `rg` for "BUG-189", "10ms floor", "import graph floor", "R0"–"R6" across `docs/` and
the memory directory — fix or tombstone every stale assertion; this doc's Status header updated to
SHIPPED-with-numbers; (3) `python3 scripts/gen_docs_index.py` if any doc was added/renamed;
(4) a Deferred-item note: R4's revival trigger now carries the measured residual main-pass share
(the number D2's trigger needs).
Gate — positive: both backlog Status lines updated; `rg -n 'BUG-189' docs/` shows no line
asserting the old floor as current; negative: none. Demo: the before/after table (P0's numbers vs
P5's), read by the orchestrator and left for Peter — L2.
Forbidden moves: editing generated boards by hand; claiming BUG-190 fixed without the BrainStem
re-measure showing it. Test scope: docs-index freshness test via the default sweep.
Dependencies: P1–P4.

**P5 landed 2026-07-17.** Full re-measure on the fully-landed tree (all of P0–P4 + P3b), AMG GT3,
two consecutive unprofiled runs per resolution: @3840×2160 GPU p50 9.454ms / 9.449ms (down from the
P0 baseline 13.554ms — ~4.1ms/~30% drop, matching P3b's numbers within run-to-run noise); @1920×1080
GPU p50 5.744ms / 5.716ms (down from the P0 baseline 9.830ms — ~4.1ms/~42% drop, a bigger
proportional win than @4K since the removed IBL-convolution cost was resolution-independent). A
profiled sanity run at both resolutions confirms `render_scene`'s tag now carries a single pass row
in steady state (no separate `shadow`/`ibl prefilter`/`ibl irradiance` rows survive — both fully
gated away), i.e. the entire residual is main pass: D1b's ~54%-of-render_scene forecast is now
effectively 100%, because everything else on a static scene is gated to zero. BrainStem
(`--warmup-frames 30`, P4's flag): GPU p50=4.003ms p95=8.174ms (healthy); CPU-encode-wall
p50=20.330ms (only ~4-5% better than P0's uncommitted 21.4ms pre-P4 measurement) — confirms P4's own
finding that the repaired `format!`/scan pattern was real but not BrainStem's dominant cost; the
remaining ~20ms is a named, unattributed follow-up in `docs/BUG_BACKLOG.md`'s BUG-190 entry, not
re-opened as a fix attempt here (D3/D3b scope). Supersession sweep: `rg` for "BUG-189", "BUG-190",
"BUG-197", "10ms floor", "import graph floor", "R0"–"R6", "RENDER_SCENE_PERF" across `docs/` found
no stale assertion of the old unfixed floor — the two other hits (`PERF_BUDGET_GATE_DESIGN.md` D7's
"first customer is BUG-189's ~10ms floor" and `GLTF_ANIMATION_DESIGN.md`'s BUG-190 cross-reference)
are both correctly-framed historical record of why those decisions were made, not present-tense
claims — left as-is. Memory directory (`~/.claude/projects/.../memory/`) swept the same way: zero
hits on any of the search terms, nothing to fix there. `docs/README.md`'s generated line for this
doc was refreshed via `scripts/gen_docs_index.py` to reflect the new Status header (no doc
added/renamed, so this was the only regen trigger). Default workspace sweep
(`cargo nextest run --workspace`) green: 3450 passed, 12 skipped, including the docs-index
freshness test.

## §. Decided — do not reopen
1. Tonight = R0+R1+R2+R3+R5; R4 and R6 deferred with named triggers (D1/D2).
2. BrainStem/BUG-190 is diagnosis-only tonight; no fix built against a guess (D3).
3. No ablation flags; the split comes from per-label unmatched-span rows, import mode only,
   landed as PERF_BUDGET_GATE_DESIGN.md's D8 amendment (D4).
4. Dirty signal = per-slot write generations, conservative default (non-declaring nodes always
   bump), truthful-declaration contract per node (D5/I3).
5. Shadow cache key = D6's exact component list including the rebuild epoch; never serve on any
   mismatch (D6/I1).
6. IBL gating is producer-first within one phase; the ~1410 doc comment gets rewritten, not
   contradicted (D7/I2).
7. Perf claims from unprofiled runs only; import mode stays report-only, no baselines (D8/I5).
8. No phase waits on A4 or SCENE_SETUP_PANEL; serial landing in one worktree slot (D9/D10).

## §. Deferred
- **R4 — indexed mesh rendering** (kill the 3.84× vertex amplification; `Array` index port,
  `draw_indexed` in manifold-gpu, reconciliation over every flat-layout-assuming mesh consumer).
  Revive: own Fable/Opus design session — the trigger has now fired. **P5's final re-measure
  (2026-07-17, AMG GT3, fully-landed tree) records the residual main-pass share as ~100% of
  render_scene's own GPU time** — ~9.45ms @3840×2160 GPU p50, ~5.73ms @1920×1080 GPU p50 — because
  every other pass (shadow, IBL) is now gated to zero on a static scene, confirmed by a profiled run
  showing a single unlabeled `node.render_scene` pass row at both resolutions (no earlier forecast
  language remains; this is the measured number, not a prediction). Revival is due: schedule the
  design session. The supervised-session requirement (D1, D2) is unchanged.
- **R6 — GPU culling.** Revive: multi-GLB merged scenes routine (SCENE_SETUP_PANEL P4 shipped)
  AND P5 shows main-pass draw cost dominating; needs graph-side AABB infra that doesn't exist.
- **Project-mode unmatched-span label breakdown** (mirror of P0's import-mode change). Revive:
  next time a project-mode attribution run's "compositor/untagged" row is too coarse to assign
  blame — same one-line change, but it amends that design's D6 and shouldn't happen unattended.
- **Generation-signal adoption beyond tonight's three consumers** (e.g. gating other heavy
  consumers, memo-skip integration). Revive: next perf campaign; the API is deliberately tiny
  until a second real customer exists.
