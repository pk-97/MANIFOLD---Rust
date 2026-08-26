# Cinematic Scene Tail — DoF + motion blur back into 3D scene graphs

**Status:** IN PROGRESS — P0 executed 2026-08-26 (BUG-136 (motion blur no visible effect) root-caused: no code defect; see the audit addendum) · P1–P3 open · k3 (lead)
**Prerequisites:** none (all atoms shipped; BUG-136 (motion blur no visible effect) open is P0 of this doc)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

GLB-imported 3D scenes have no depth-of-field or motion-blur nodes at all — the import graph was made SSAO-only in `72135693` (2026-07-12), one day *before* the dof-polish batches landed the fixes (CoC dilation, occlusion-aware bokeh gather) for the "blocky 2001 blur" look that motivated the removal. Peter has never seen the polished chain. Meanwhile the Scene Setup panel surfaces `focus_distance` / `f_stop` / `shutter_angle` rows that write lens params nothing consumes, and `node.motion_blur` is live-tracked as BUG-136 (no visible effect despite proven-correct inputs).

Peter, verbatim: "we will need pre-existing 3D scenes to be upgraded so these nodes are now in their graphs if they're missing. The Scene UI shows the sliders and params but they do nothing at the moment." And the investigation constraint: "I don't want you using probes unless you are at an extreme dead end. Probes are very very very slow and expensive."

Binding constraints: **persistence** (every existing 3D project must load and gain the tail — a load-migration, not a manual rebuild) and **hot path** (the tail adds GPU dispatches per frame; Peter's bar is "not drop render times to 2 fps", so the budget is measured, not argued).

**Companions:** [CINEMATIC_POST_DESIGN.md](CINEMATIC_POST_DESIGN.md) (the atoms + the D6 reference preset; its I2 pinhole-passthrough invariant is what makes migration safe) · [CAMERA_AND_LENS_DESIGN.md](CAMERA_AND_LENS_DESIGN.md) (`LensParams`, shutter semantics) · [RAYTRACING_DESIGN.md](RAYTRACING_DESIGN.md) D5 (traced DoF/motion blur rejected; these stay post-process).

## 1. Audit — what exists (verified 2026-08-26)

| Piece | Where | State |
|---|---|---|
| `node.camera_lens` (focus_distance / f_stop / shutter_angle / exposure_ev) | `crates/manifold-renderer/src/node_graph/primitives/camera_lens.rs:40` | Shipped, working |
| `node.coc_from_depth` (thin-lens CoC) | `primitives/coc_from_depth.rs:69` | Shipped, gpu-proofed |
| `node.coc_dilate` (3x3 neighborhood-max — the hard-cutoff fix) | `primitives/coc_dilate.rs:51` | Shipped (dof-polish batch 1, 2026-07-13) |
| `node.bokeh_gather` (occlusion-aware disc gather — the blocky-blur fix) | `primitives/bokeh_gather.rs:60` | Shipped (dof-polish batch 1, 2026-07-13) |
| `node.motion_blur` (velocity-directed gather) | `primitives/motion_blur.rs:82` | Shipped but **no visible effect live — BUG-136, OPEN, root unknown** |
| Reference wiring of the full chain | `assets/reference-presets/CinematicScene.json` (lens → coc → coc_dilate → bokeh → motion_blur → out) | Shipped, headless-renderable |
| Import graph assembly | `crates/manifold-renderer/src/node_graph/gltf_import/scene.rs:634-690` | **SSAO-only** (`ssao_gtao → bilateral_blur H/V → mix`); lens/DoF/motion absent since `72135693`; `scene.rs:720` notes the motion_blur removal under BUG-136 |
| Scene Setup lens rows (the dead sliders) | `crates/manifold-ui/src/panels/scene_setup_panel.rs:3151-3153` (`RowAddr::root(71, …)` focus_distance/f_stop/shutter_angle) | Rows write lens params; in import graphs nothing consumes them |
| BUG-136 probe state | `docs/BUG_BACKLOG.md:1240-1272` | Inputs proven correct headlessly (velocity nonzero, shutter at atom, 30/30 frames); **output pixels never diffed against input** — the unprobed gap; drag-propagation suspect killed by Peter 2026-08-26 (camera was moving) |
| Migration machinery | `crates/manifold-io/src/migrations/` (`scene_transform_v1120.rs`, `param_storage_v14.rs`) | Two precedents, versioned, load-time |

Classification: every atom **exists**; the import-graph tail and the migration are **one wire away** (template + injector); **genuinely new**: nothing. This design is wiring plus one bug fix.

**Audit addendum (2026-08-26, P0 execution — the BUG-136 (motion blur no visible effect) verdict).** The bug was never in the code. SceneLadders.manifold has ONE timeline layer whose `gen_params.graph` — the graph the renderer actually builds (`layer.rs:133-136`, graph-home unification) — contains `camera_lens` + `render_scene` and **no motion_blur, no DoF chain at all**. The five chain-bearing graphs live in `embeddedPresets[9-12,16]`, which no layer instantiates. The July repro is explained by the pre-BUG-237 (scene-setup scrub writes dead) bound-row write path (fixed 2026-07-18); today's repro by the missing chain. The fused-route suspect died structurally: every shipped motion_blur has a gather (`variable_blur`) or group upstream, never a pointwise producer, so the freeze compiler refuses the chain (confirmed in the new gpu proof) — the `camera_ext` zero-fill path is unreachable. New gpu proof `motion_blur_visibility` (raw route): blur visible under motion with shutter 180, bit-clean static control — kernel, wiring, velocity, and lens-derived shutter all healthy. **Consequence for D3: the migration MUST cover `timeline.layers[].gen_params.graph`, not only `embeddedPresets` — the layer graphs are where the chain is missing.**

Section 2.5 audit (DECOMPOSING_GENERATORS.md section 2.5 (primitive audit)): no new primitive is proposed; the chain is the shipped CINEMATIC_POST atoms exactly as CinematicScene composes them.

## 2. Decisions

**D1 — Fresh imports get the full polished tail, always, with neutral lens defaults.**
Import assembly wires `camera_lens → coc_from_depth → coc_dilate → bokeh_gather → motion_blur` after the existing SSAO mix, templated node-for-node on CinematicScene.json. Defaults are the neutral lens (`f_stop` = 1000, `shutter_angle` = 0), which CINEMATIC_POST I2 (pinhole pass-through) guarantees is a bit-clean pass-through — a fresh import renders byte-identical to today until Peter dials the lens.
Rejected: a per-scene enable toggle — the neutral passthrough already *is* the off state; a toggle adds an identity/flag system for zero behavioral difference (zero-new-systems test, DESIGN_AUTHORING.md section 3 (shaping the architecture)).
Rejected: reinstating the pre-polish `variable_blur` DoF chain — that is the exact build Peter remembers as "a blocky blur filter from 2001"; `coc_dilate` + `bokeh_gather` replaced it and he has never seen the replacement.

**D2 — BUG-136 is fixed before the tail ships anywhere (P0).**
Shipping a tail whose last atom does nothing would repeat the dead-sliders failure one level down. Investigation is lead-seat semantic review of the fused-vs-standalone routing seam first (the surviving suspect family: codegen mis-selecting a pass-through kernel, or the output not reaching `final` in the live graph), then **one** instrumented headless run that diffs `motion_blur` output pixels against its input on an orbiting camera — the check the 2026-07-13 probe session never ran (it verified inputs, declared them clean, and stopped). No live-app probe loops — Peter's directive, quoted above.
Rejected: live-app println probing as the method of first resort — banned by Peter's directive; the headless output-diff is cheaper and is also the regression test.

**D3 — Existing 3D projects gain the tail by load-migration, not by hand.**
New versioned migration `scene_cinematic_tail_vNNNN` in `crates/manifold-io/src/migrations/`, shaped like `scene_transform_v1120.rs`: any scene graph containing `node.render_scene` but no `node.bokeh_gather`/`node.motion_blur` consumer gets the D1 tail injected with neutral defaults — **where "any scene graph" means BOTH `embeddedPresets[].def` AND every `timeline.layers[].gen_params.graph` (the audit addendum's convicted case: layer graphs are per-instance overrides and the renderer builds THEM, so a preset-only migration leaves the playing layers untouched).** Round-trip is the gate (save → reload → modulate after reload), per DESIGN_DOC_STANDARD.md section 5 (round-trip gate rule).
Consequences, stated honestly: migration must find the same insertion point the import assembler uses in graphs it didn't build; graphs Peter has hand-edited since import may not match the expected shape. Default: insert immediately upstream of the graph's `final` sink regardless of SSAO presence; if no `final` sink is found, skip that scene loudly (load-time repair toast, the BUG-079 (missing-preset-fails-silently) pattern) — never silently drop, never invent a second insertion heuristic.

**D4 — The dead sliders are fixed by construction, then guarded.**
The Scene Setup rows already write the right lens params; once D1/D3 give those params consumers, the rows go live with zero UI changes. The guard is a test: for an import-assembled graph and a migrated graph, every surfaced lens param (`focus_distance`, `f_stop`, `shutter_angle`) has a downstream consumer path to `final`.
Rejected: hiding the rows when consumers are absent — masks the bug class instead of killing it, and adds UI state for a condition that should not exist.

**D5 — DoF runs at half resolution; motion blur full-res, single dispatch.**
`bokeh_gather` and the CoC chain at half-res (upsampled on composite — large defocus is low-frequency by definition); `motion_blur` is one full-res gather. Frame-cost budget: the whole tail ≤ 3 ms at 1920×1080 on the rig, measured with `MANIFOLD_RENDER_TRACE=1` (DESIGN_DOC_STANDARD.md section 5 (content-thread work gate)). If the measurement breaks budget, the fix is resolution, not deleting atoms.
Consequences, stated honestly: half-res DoF can halo on razor-thin in-focus silhouettes against deep defocus; accepted — the alternative (full-res gather) doubles cost for a defect visible only in stills at extreme settings.

**Plausible-wrong architecture, forbidden by name:** writing a *new* DoF or motion-blur kernel "tuned for imports" instead of wiring the shipped atoms. Every kernel in this chain has gpu-proofs against CPU references; a new one restarts that proof burden and forks the look. The second forbidden turn: an `Arc<Mutex>`-cached "scene has tail" flag — the graph itself is the record; query it, don't mirror it.

## 3. Chain topology (committed seam)

```
render_scene.out(color) ──┬──> ssao_mix (existing) ──> bokeh_gather.in
render_scene.depth ───────┴──> coc_from_depth.depth
lens.out ─────────────────────> coc_from_depth.camera
coc_from_depth.out ───────────> coc_dilate.in
coc_dilate.out ───────────────> bokeh_gather.coc
bokeh_gather.out ─────────────> motion_blur.in
render_scene.velocity ────────> motion_blur.velocity
lens.out ─────────────────────> motion_blur.camera
motion_blur.out ──────────────> final
```

This is CinematicScene.json's wiring transcribed; the import assembler and the migration MUST produce this same shape (the migration's insertion point is the `final` upstream edge, D3). Half-res marker on the CoC/bokeh leg per D5 — ⚠ VERIFY-AT-IMPL: the exact half-res mechanism CinematicScene uses (read the preset's size/resolution params, do not invent one).

## 4. Invariants & enforcement

- **I1 — Neutral lens is a bit-clean pass-through through the whole injected tail.** Enforcement: gpu_test — import-assembled graph with default lens, noise-texture scene, `motion_blur.out` byte-compared against the SSAO-mix output (extends CINEMATIC_POST I2 (pinhole pass-through) to the assembled graph, not just the reference preset).
- **I2 — No dead lens params.** Enforcement: test `scene_lens_params_have_consumers` over an import-assembled graph and a migrated fixture graph (D4).
- **I3 — Fused == unfused for the tail.** Enforcement: existing per-atom freeze proofs, run via `scripts/gpu_proofs_gate.py` at P1/P2 gates (GPU path touched by definition).
- **I4 — Tail frame cost ≤ 3 ms at 1920×1080.** Enforcement: `MANIFOLD_RENDER_TRACE=1` run on the acceptance scene at P1; number reported in the landing, >20 ms any-frame fails the gate per the standard's content-thread rule.
- **I5 — Migration never silently skips.** Enforcement: the no-`final`-sink path raises the load-time repair toast (BUG-079 (missing-preset-fails-silently) pattern) and a test asserts the toast fires on a doctored fixture.

## 5. Phasing

### P0 — BUG-136 (motion blur no visible effect) root cause + fix
- **Entry state:** BUG-k57 (BUG-136 — motion blur no visible effect) open; `docs/BUG_BACKLOG.md:1206-1272` read, including both addenda.
- **Read-back:** restate the three killed suspects, the surviving suspect family (D2), and the fact that the 2026-07-13 probes verified inputs only.
- **Deliverables:** the fix (wherever the semantic review convicts); gpu-proofs value test if a kernel or routing table changed; regression test `motion_blur_output_differs_under_orbit` — headless render, time→orbit wired, computed pixel-diff between `motion_blur.in` and `.out` above a stated threshold. **The test must fail on pre-fix code.**
- **Gate:** `scripts/gpu_proofs_gate.py` green; regression test red-then-green.
- **Demo:** none — L1 (P3 is the look-pass).
- **Forbidden moves:** probe loops (Peter's ban); fixing the symptom by boosting `max_blur_px` defaults; touching `shutter_angle` semantics.
- **Test scope:** `-p manifold-renderer` + gpu-proofs.

### P1 — Import-graph tail (fresh imports)
- **Entry state:** P0 landed; `rg node.motion_blur crates/manifold-renderer/src/node_graph/gltf_import/scene.rs` shows only the removal note.
- **Read-back:** D1, D5, section 3 (chain topology); CinematicScene.json read end-to-end (the template); `scene.rs:600-730` (the SSAO block it appends after).
- **Deliverables:** tail injection in `gltf_import/scene.rs`; I1 and I2 tests; I4 measurement.
- **Gate:** `graph-tool validate` on a regenerated import graph; I1 byte-compare green; I4 number reported; `scripts/gpu_proofs_gate.py` green. Held-out input: `tests/fixtures/rt/apricot_tl05.glb` (never developed against in this design).
- **Demo:** L2 — headless PNGs of the held-out scene at f_stop 1000/2.8/1.4 and shutter 0/180, produced for Peter to look at; agent gate is computed region-statistics (defocused-region variance below, in-focus region above, stated thresholds).
- **Performer gesture:** orbit the camera hard mid-set with shutter at 270° — motion must smear, not strobe (asserted via the P0 regression test on the imported graph).
- **Forbidden moves:** reusing the pre-polish `variable_blur` chain; inventing a half-res mechanism instead of copying the preset's; adding a toggle.
- **Test scope:** `-p manifold-renderer` + gpu-proofs.

### P2 — Migration for existing projects
- **Entry state:** P1 landed (the migration targets the same topology); `crates/manifold-io/src/migrations/scene_transform_v1120.rs` read as the shape precedent.
- **Read-back:** D3 including the skip-loudly default; I5.
- **Deliverables:** `scene_cinematic_tail_vNNNN` migration; fixture = a saved pre-tail 3D project (SceneLadders.manifold or equivalent, committed as fixture if licensing allows); I5 toast test.
- **Gate:** round-trip — load fixture → tail present → save → reload → modulate `f_stop` after reload and assert CoC output changes (computed, not eyeballed); I2 test passes on the migrated graph; `cargo nextest run -p manifold-io -p manifold-renderer`.
- **Demo:** L2 — before/after headless PNGs of the fixture scene at f_stop 2.8 for Peter.
- **Forbidden moves:** silent drop on unresolvable graphs; hand-editing any `.manifold` ZIP (project_tool rule); migrating graphs that already have a tail (idempotence — assert second load is a no-op).
- **Test scope:** `-p manifold-io -p manifold-renderer`.

### P3 — Look-pass on the rig
- **Entry state:** P0–P2 landed on main.
- **Deliverables:** Peter's session on his own scenes; verdict recorded in this doc's status line.
- **Gate:** Peter, L4. No agent gate.
- **Demo:** the rig.

## 6. Decided — do not reopen

1. The tail is the shipped CINEMATIC_POST atoms (lens → coc → dilate → bokeh → motion_blur), never a new kernel (D1 and the forbidden-turn paragraph in section 2 (Decisions)).
2. Neutral lens defaults = pass-through = no toggle (D1).
3. BUG-136 blocks the tail (D2); headless output-diff is the oracle, live probe loops are banned (Peter).
4. Existing projects migrate at load, skip-loudly, idempotent (D3).
5. Dead sliders are fixed by giving params consumers, plus the I2 guard — never by hiding rows (D4).

## 7. Deferred

- **Traced DoF / motion blur** — RAYTRACING_DESIGN D5 already defers it; revival trigger: measured spare ray budget after RT P3.
- **Per-scene tail removal (a real off switch)** — revival trigger: I4's 3 ms budget ever measured binding on the heaviest show project; until then neutral defaults are the off state.
- **Bokeh shape controls (blade count, anamorphic squeeze)** — atoms don't support them; revival trigger: Peter asking for a specific lens character after the P3 look-pass.
