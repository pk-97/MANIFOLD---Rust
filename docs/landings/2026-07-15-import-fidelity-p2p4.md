# IMPORT_FIDELITY F-P2 + F-P4 — landed 2026-07-15 @ `9b96fd82` (pending push to origin/main)

**Branch:** `feat/import-fidelity-fp2` (F-P2, `c778dbe3`) + `feat/import-fidelity-fp4` (F-P4, `9e5c5432` + fix `a96e8167`, branched off F-P2's tip since F-P4 wires the ports F-P2 built), merged into `main` as one batch per `.claude/GIT_TREE_DISCIPLINE.md` §2c.
**Level reached:** L1 (numeric gpu-proofs + unit gates, run independently by both the worker and the orchestrator, all green) / target L4 (§10) — L4 is Peter's click-script below, not yet run by him.
**Doc status line (quoted verbatim):** `IN PROGRESS · F-P1 + F-P3 SHIPPED 2026-07-15 (orchestrator session 1 of 3, landing report \`docs/landings/2026-07-15-import-fidelity-p1p3.md\`) · F-P2 + F-P4 SHIPPED 2026-07-15 (orchestrator session 2 of 3, landing report \`docs/landings/2026-07-15-import-fidelity-p2p4.md\`) · approved by Peter 2026-07-15 ("Approved") · authored 2026-07-15 · Fable 5 (his product calls are quoted in the intro, D7, and D8; glass/F-P5, pure-black base, and sun coherence added same day at his direction). Execution: 3 orchestrator sessions — (1) F-P1 ∥ F-P3 DONE, (2) F-P2 + F-P4 DONE, (3) F-P5 next.`

## Gate results (verbatim)

F-P2, re-run by the orchestrator against the worker's committed state (`cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs render_scene -- --test-threads=1`):
```
running 35 tests
...
test render_scene_map_set::emissive_map_adds_after_lighting_with_zero_incident_light ... ok
test render_scene_map_set::mr_map_blue_channel_drives_f0_via_metallic ... ok
test render_scene_map_set::normal_map_tilts_the_lit_value_by_the_cotangent_frames_predicted_amount ... ok
test render_scene_map_set::occlusion_map_darkens_diffuse_ibl_term_only ... ok
test render_scene_map_set::unwired_normal_map_reproduces_the_pre_fp2_lambert_formula_exactly ... ok
...
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out
```
Negative gate: `rg -n 'texture_flags2' render_scene.wgsl render_scene.rs` shows the shader-side reads (`u.texture_flags2.x/y/z`) confined to `resolve_mr`/`resolve_occlusion`/`resolve_emissive` (lines 579/591/603); all other hits are struct definition, assignment, and comments — no ad-hoc reads elsewhere. `cargo clippy -p manifold-renderer -p manifold-gpu -p manifold-core -- -D warnings`: clean.

F-P4, re-run by the orchestrator (`cargo test -p manifold-renderer --lib node_graph::gltf_import` then `--features gpu-proofs --lib node_graph::gltf_import -- --test-threads=1`):
```
test result: ok. 10 passed; 0 failed; 1 ignored (non-GPU)
...
test node_graph::gltf_import::tests::damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate ... ok
test node_graph::gltf_import::tests::amg_gt3_glb_imports_and_renders_without_error_if_present ... ok
test node_graph::gltf_import::tests::round_trip_preserves_map_wires_and_sun_coherence_bindings ... ok
test node_graph::gltf_import::tests::sun_macros_bind_both_the_light_and_the_envmap_disc_direction ... ok
test node_graph::gltf_import::tests::imports_all_map_kinds_with_correct_color_spaces ... ok
test node_graph::gltf_import::tests::orm_packed_occlusion_and_mr_share_one_texture_source_node ... ok
test node_graph::gltf_import::tests::over_featured_material_reports_clearcoat_transmission_and_blend_downgrade ... ok
test result: ok. 12 passed; 0 failed; 3 ignored
```
`cargo clippy -p manifold-renderer -- -D warnings`: clean. `cargo run -p manifold-renderer --bin check-presets`: `57 presets: 57 ok, 0 failed`. Fixture attribution confirmed in `tests/fixtures/gltf/README.md` (DamagedHelmet, CC-BY 4.0, Khronos glTF-Sample-Assets). `git ls-files tests/fixtures/gltf/` confirms the AMG GT3 `.glb` is NOT tracked.

**Full workspace sweep, run once at landing in the warm main checkout, against the merged state:**
- `cargo clippy --workspace -- -D warnings`: clean (only pre-existing Obj-C deprecation notices from `manifold-media`'s native build, not Rust warnings).
- `cargo nextest run --workspace`: `3380 tests run: 3380 passed (1 leaky), 12 skipped` — 35.07s.
- `cargo deny check bans`: `bans ok`.
- `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs -- --test-threads=1` (full binary, serialized, against the merged main state): `44 passed; 0 failed`.
- `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::gltf_import -- --test-threads=1` (against merged main state): `12 passed; 0 failed; 3 ignored`.

## Deviations from brief

1. **F-P2 repurposed a dead binding rather than adding a fourth new one.** The doc's D3 prose reads as "three new bindings" (mr/occlusion/emissive) alongside the existing normal-map slot; the worker found that `render_scene`'s normal-map binding (slot 4) and its `roughness_map`/`metallic_map` bindings (slots 5/7) were permanently-dead P8 stubs — declared, never wired, flags never set, within `render_scene` specifically (single-object renderers use the same names but a separate binding table, untouched, per D1). Repurposing slot 4 for the new tangent-space `resolve_normal` carries zero regression risk (nothing ever read the old semantics) and avoids a needless binding-table renumber. Slots 5/7 stay dead as documented tech debt rather than being removed, since removing them would force renumbering every binding after them for zero functional gain. This is a reading of "one wire away from existing" (the doc's own audit classification for the port plumbing) rather than a departure from D3's intent.
2. **`normal_texture.scale`/`occlusion_texture.strength` parsed but not wired end-to-end.** D5 has both fields parsed by the loader; D4 says scale "imports as a multiplier," but no shader-ABI param carries either value through F-P2's resolve functions today. Non-default values become a report line (D9 doctrine — never silently dropped) rather than a visible effect. Both fixtures used for verification (DamagedHelmet, AMG) default to 1.0/no-strength-override, so this is inert on the assets in hand. Recorded as Deferred #7 in the design doc with its trigger, rather than left as an undocumented gap.
3. **F-P4 worker process collision.** The F-P4 worker agent initially misread its instructions and spawned a nested sub-agent to do the phase's work instead of doing it directly; the orchestrator caught this from the returned report's shape (a "launched in background" summary with almost no tool calls) and redirected it. The worker could not cleanly stop the sub-agent it had spawned (an ownership mismatch on the stop call) and instead read the sub-agent's in-progress diff at each step, independently verified or rewrote every piece itself, deduplicated a duplicate test set the sub-agent had also written, and found+fixed a real bug in a convergence-poll loop (a missing non-black-fraction gate that let the DamagedHelmet render gate falsely "converge" on transient all-black frames before background texture decode finished) along the way. The orchestrator re-ran every gate independently against the final committed state (`git diff HEAD` empty, working tree matched exactly) before trusting any of it — see the gate output above, all executed fresh by the orchestrator, not relayed. No process residue reached the landed commits.

No other deviations. Neither phase touched `render_mesh`/`render_copies`, grew `MeshVertex`, added a new port type, added a channel-select mode flag on a shared resolve function, added `Arc<Mutex>`, implemented `AlphaMode::Blend` (F-P4 correctly stopped at the Mask-plus-report-line stopgap), or produced any PNG artifact.

## Shortcuts confessed (rolled up from phase reports)

- F-P2: `normalTexture.scale` not wired (see Deviation 2 above — the worker's own confessed shortcut, now a tracked Deferred item); dead bindings 5/7 left in place rather than renumbered (Deviation 1, not a shortcut so much as a scoped non-change).
- F-P4: none reported beyond the additive report-line behavior for `normal_scale`/`occlusion_strength` (Deviation 2), which the worker framed as D9-compliant rather than a shortcut — the orchestrator agrees with that framing since nothing is silently dropped.

## Verification debt

- **VD (carried from F-P1+F-P3 landing):** F-P1's diffuse-irradiance gate is a generous-band sanity check, not the doc's originally specified uniform-white-env value-level test. Unaffected by this landing.
- **VD (carried):** L4 (Peter, live in-app) not yet reached for F-P1/F-P3 or for this landing's F-P2/F-P4 — expected at this stage; burns down when Peter runs the click-script below (covers both landings, since F-P2/F-P4 are only observable once wired together and F-P1/F-P3's click-script items are unaffected by this landing).
- **VD (opened):** Deferred #7 (`normal_texture.scale` / `occlusion_texture.strength` unwired) — see Deviation 2. Low risk: both held-out fixtures default to neutral values, and the report line makes the gap visible at import time per D9 rather than hiding it.
- None else opened.

## Click-script for Peter (≤2 minutes)

1. Import the AMG GT3 `.glb` (drag-and-drop or the import menu, whichever the app currently exposes). **Expect:** the environment is pure black with a few bright horizontal light streaks (softbox default, no longer the grey gradient studio) — the chrome body panels should show sharp, bright streak reflections against the void, livery/panel detail should read from the base-colour map, and headlights should read noticeably brighter than the body (emissive glow, bloom-ready).
2. On the imported scene's sun-direction macro (search for "Sun X/Y/Z" or similar on the card), sweep it on a fader. **Expect:** the sun disc in the environment, the AMG's cast shadow, AND its specular highlight/reflection all move together in the same direction — one gesture, three coherent effects (this is D7's sun-coherence binding).
3. Look at the AMG's windows/windscreen. **Expect:** they render as opaque grey/cutout panels, not glass — this is F-P4's intentional stopgap (BLEND materials import as Mask-cutout with a report line); F-P5 (next session) replaces this with real sorted glass.
4. Save the project, close it, reopen it. **Expect:** the AMG still shows its maps wired exactly as before closing (chrome still reflective, livery still textured, glow still present) — this is the round-trip gate; nothing should look flatter or greyer after reload.
5. Open the import report for the AMG (wherever the app surfaces it — a panel, a log, a summary dialog). **Expect:** lines calling out any clearcoat-painted or transmission (glass) materials the importer couldn't fully map yet, plus a line noting BLEND materials were downgraded to cutout — nothing should be silently missing, everything unmapped should be named.
