# IMPORT_FIDELITY F-P5 — landed 2026-07-15 @ `61400029` (pending push to origin/main)

**Branch:** `feat/import-fidelity-fp5`, orchestrator session 3 of 3 (execution ran in-session, no separate worker/orchestrator split for this phase).
**Level reached:** L1 (numeric gpu-proofs + unit gates, all green) / target L4 (§10) — L4 is Peter's click-script below, not yet run by him.
**Doc status line (quoted verbatim):** `SHIPPED · F-P1 + F-P3 SHIPPED 2026-07-15 (orchestrator session 1 of 3, landing report \`docs/landings/2026-07-15-import-fidelity-p1p3.md\`) · F-P2 + F-P4 SHIPPED 2026-07-15 (orchestrator session 2 of 3, landing report \`docs/landings/2026-07-15-import-fidelity-p2p4.md\`) · F-P5 SHIPPED 2026-07-15 (orchestrator session 3 of 3, landing report \`docs/landings/2026-07-15-import-fidelity-p5.md\`) · approved by Peter 2026-07-15 ("Approved") · authored 2026-07-15 · Fable 5 (his product calls are quoted in the intro, D7, and D8; glass/F-P5, pure-black base, and sun coherence added same day at his direction). Execution: 3 orchestrator sessions — (1) F-P1 ∥ F-P3 DONE, (2) F-P2 + F-P4 DONE, (3) F-P5 DONE — all phases shipped.`

## Gate results (verbatim)

Focused unit tests (`cargo nextest run -p manifold-renderer --lib gltf_import` and `--lib render_scene`):
```
Summary [   0.416s] 11 tests run: 11 passed, 1236 skipped   (gltf_import)
Summary [   0.399s] 20 tests run: 20 passed, 1227 skipped   (render_scene)
```

New F-P5 gpu-proofs suite (`cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs render_scene_glass`):
```
running 4 tests
test render_scene_glass::see_through_blend_matches_straight_alpha_over_formula ... ok
test render_scene_glass::blend_object_fully_behind_opaque_contributes_nothing ... ok
test render_scene_glass::swapping_pane_positions_swaps_the_blend_order ... ok
test render_scene_glass::blend_material_casts_no_shadow ... ok
test result: ok. 4 passed; 0 failed
```

Full `render_scene` gpu-proofs suite, proving zero regression on every pre-existing gate (`cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs render_scene`):
```
test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out
```

Negative gate — no OIT/per-triangle-sort tokens on any touched file (`rg -i 'oit|per_triangle_sort' <touched files>`): zero hits (exit 1).

`cargo clippy -p manifold-renderer -p manifold-gpu --all-targets --features gpu-proofs -- -D warnings`: clean.
`cargo run -p manifold-renderer --bin check-presets`: `57 presets: 57 ok, 0 failed`.
`cargo run -p manifold-renderer --bin gen_node_catalog`: regenerated `docs/node_catalog.json` (the new `Blend` enum value on all four material atoms' `alpha_mode` param) — `docs/NODE_CATALOG.md`'s generated block was unaffected.

**Full workspace sweep, run once at landing in the warm worktree (re-verified in the main checkout before push):**
- `cargo nextest run --workspace`: `3381 tests run: 3381 passed, 12 skipped` — 8.48s.
- `cargo deny check bans`: `bans ok`.

## Deviations from brief

1. **Bounding-box centroid approximated by object translation.** D8 says "sorted back-to-front by view-space depth of the transformed bounding-box centroid." No per-object AABB is tracked anywhere in the graph today (the audit found none), and building CPU-side mesh-bounds infrastructure to compute a true local-space AABB is out of this phase's scope — it would require either a GPU readback of vertex data every frame (a real per-frame cost proportional to mesh density, conflicting with hot-path discipline) or new infra to cache it (a scope expansion the brief didn't authorize). The sort key used instead is the object's model-matrix translation (its `node.transform_3d` position, or the origin if unwired) projected onto the camera's forward axis — the common "sort by object pivot" implementation used by most engines for inter-object transparency ordering, and exactly sufficient for D8's own stated use case (two stacked glass panes, each a separate object). The `swapping_pane_positions_swaps_the_blend_order` gate proves this is correct for that case, matching the formula exactly in both configurations.
2. **`DepthMsaaPassDesc` grew a `second_pass` field rather than a new encoder entry point.** The design body doesn't commit an exact seam for this; the chosen shape (an optional second draw group, drawn in the same encoder pass right after the first with a switched depth-stencil state) reproduces byte-identical behavior when `None` and required touching the one other caller (`draw_instanced_depth_msaa_batch`, used unmodified by `render_mesh`/`render_copies` — D1 scope fence respected) to pass `second_pass: None` explicitly. No new pass, no new clear, no per-pixel cost when a scene has no `Blend` objects.
3. **Pipeline cache key widened from `(MaterialKind, bool)` to `(MaterialKind, bool, bool)`.** Mechanical consequence of adding a `blend` dimension to `pipeline_for`; `prewarm_pipelines` was extended to warm all 16 variants (4 kinds × 2 emit_velocity × 2 blend) per the existing BUG-037 discipline, so a live project's first glass draw is a cache hit, not a first-use compile stall.
4. **`over_featured_material_reports_clearcoat_transmission_and_blend_downgrade` test rewritten, not just extended.** F-P4's test asserted the Mask-downgrade report line this phase deletes; renamed to `over_featured_material_reports_only_clearcoat_and_maps_transmission_to_blend` and rewritten to assert the new behavior (one report line for clearcoat only, plus the built material's `alpha_mode`/`color_a` reflecting the transmission-folded Blend mapping) — a like-for-like replacement of a test whose asserted behavior this phase intentionally supersedes, not scope creep.

No other deviations. F-P5 did not touch `render_mesh`/`render_copies` behavior (both keep their old Opaque-coverage fallback for a `Blend` material, explicitly), did not grow `MeshVertex`, did not add a new port type, did not implement OIT or per-triangle sorting, did not write depth in the blend pass, did not premultiply in the shader, and produced no PNG artifact.

## Shortcuts confessed

- The bounding-box-centroid approximation (Deviation 1 above) — the one genuine shortcut this phase takes, confessed there with its reasoning and the gate that bounds its risk.
- No other shortcuts, hard-codes, or unverified assumptions.

## Verification debt

- **VD (carried from F-P1+F-P3/F-P2+F-P4 landings):** L4 (Peter, live in-app) not yet reached for any IMPORT_FIDELITY phase — expected at this stage; burns down when Peter runs the combined click-script (this landing's steps below, plus the earlier landings' steps, since they're only fully observable together on the AMG asset).
- **VD (opened):** the bounding-box-centroid approximation (Deviation 1) is a genuine simplification versus the design body's literal wording, though it is priced and gated. Trigger to revisit: a hero asset whose glass objects have off-center local origins such that the translation-based sort visibly mis-orders (not the AMG's windows — those are centered on their own pivots).
- None else opened.

## Click-script for Peter (≤2 minutes)

1. Import the AMG GT3 `.glb` (drag-and-drop or the import menu). **Expect:** same look as the F-P2+F-P4 landing (pure black void, softbox streaks, chrome reflections, livery/panel detail, glowing headlights) — this phase changes only the windows.
2. Look at the AMG's windshield and side windows. **Expect:** they now read as GLASS, not opaque cutout panels — you should see through them (the cockpit interior, or the void/softbox streaks behind, tinted by the glass colour) rather than a flat grey cutout shape.
3. Orbit the camera so a near window and a far window (e.g. windshield and a rear side window) overlap on screen. **Expect:** the nearer glass surface visibly composites over what's behind it (the far window, the cockpit, or the void) — no popping, flickering, or one pane disappearing behind the other as the camera moves.
4. On the imported scene's material card for a window (search for "Opacity" on one of the glass objects), sweep it from 1.0 down toward 0.0 on a fader. **Expect:** the window goes from solid-looking to fully see-through (ghost) smoothly, live, with no visual glitching.
5. Confirm the AMG still casts a normal shadow from its body onto the ground, and that the glass panels do NOT cast their own separate shadow shapes. **Expect:** shadow silhouette matches the car's opaque body only — no shadow "biting" from the window openings.
6. Save the project, close it, reopen it. **Expect:** the windows are still glass (not reverted to opaque cutout) and the Opacity fader from step 4 still works after reload.
7. Open the import report for the AMG. **Expect:** the BLEND-downgraded-to-Mask and transmission-report-only lines from the previous landing are GONE — those features are now actually mapped, not just reported as missing.
