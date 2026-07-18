# GLB Import Optimization Guide — heavy photoscans → stage-ready scenes

**Status:** REFERENCE (living) — practical playbook, not a design doc. Written 2026-07-18 (Fable)
from a measured session optimizing `MeshAudio.manifold` (three CC0 photoscanned flowers,
0.6–1.4M tris each, 3840×2160@60, M4 Max). Numbers are that session's; re-measure per asset.

Store-bought / photoscanned GLBs arrive far heavier than a live scene can afford. This is the
order of levers that actually moved the frame, cheapest first, with the ones that *look* like
levers but aren't.

## The measurement oracle — always start here

`cargo xtask perf-soak "<project>.manifold" --seconds 10 --profile` runs a headless real-time
soak and writes `target/perf-profile/<name>-profile.json` — per-node GPU attribution for the 5
worst frames. Aggregate by `type_id` (skip `frame_index==0`, it's shader compile) to see where
the frame goes. For a stable p50/p95 A/B between two variants, run the **non**-profile form
(`--seconds 10`, no `--profile`) **back-to-back** — absolute numbers drift with thermals on an
M4 Max across sequential runs, so only trust deltas measured adjacently, never across the session.

## Lever 1 — kill SSAO / ambient occlusion (biggest single win here: ~24ms)

Every imported scene bakes a cinematic AO chain into its graph: `ssao_gtao → bilateral_blur ×2 →
mix (multiply)` (`gltf_import.rs` ~line 2351, "CinematicScene ships"). At 4K across three scenes
this cost **~24ms p50** (45.7 → 21.4ms measured). There is **no runtime toggle** today — it's
graph nodes, not a param.

**Gotcha that cost this session an hour:** the AO nodes live in the layer's own
`genParams.graph`, NOT in the embedded-preset library copy, and NOT regenerated on load. Editing
the embedded preset does nothing — the live per-layer graph is authoritative. The AO group is a
nested `group` node titled `"Ambient Occlusion"`; to strip it, splice the wire feeding its
`color` input straight to whatever consumes its `out`, then delete the group and its wires. When
a layer's `genParams.graph` is `null`, it falls back to the embedded preset def — materialize the
FULL def (keep `version`/`presetMetadata`, not just nodes+wires, or the loader rejects it).

Verify by re-profiling: `ssao_gtao` and `bilateral_blur` must read 0.0ms.

**Product gap worth closing:** a real per-scene "Ambient Occlusion" toggle (a param driving a
`switch_texture` that makes the AO branch unreachable so the runtime prunes it — pruning confirmed
at `execution_plan.rs:331`). Until that ships, AO-off is a manual graph edit per project.

## Lever 2 — mesh optimization: weld → (bake normals) → simplify → resize textures

Photoscans are geometry-bound: `render_scene` pushes every triangle through vertex processing
twice per frame (shadow pass + main pass). The realism lives in the **albedo texture**, not the
mesh, so the mesh can be cut hard if surface detail is preserved as a normal map.

Tooling is `gltf-transform` (`npx --yes @gltf-transform/cli@latest`), run outside Manifold; the
app re-imports the lighter GLB. Correct order:

1. **`weld`** — photoscans export as triangle soup (duplicated vertices per edge). Unwelded, the
   simplifier can't collapse across the gaps and you get cracks and jittering edges. Weld is
   **seam-aware** (only merges vertices matching in position AND UV), so it never fuses across a
   UV seam — the texture stays mapped.
2. **Bake normals from the ORIGINAL high-poly** (Blender/Marmoset — the one non-CLI step). This
   captures fine surface relief so a hard-decimated mesh still shades as detailed. Bake from the
   original, not the welded-down mesh.
3. **`simplify --lock-border true`** — `--lock-border` protects UV-seam/border edges so texture
   islands don't tear. `--ratio` targets a reduction; `--error` caps distortion (`--error 0.001`
   held this session's flowers to ~60% of tris because it quit early to protect the silhouette).
4. **`resize --width 2048 --height 2048`** — 2K is plenty at stage distance; dropped textures
   ~10× here (18.7 → 1.6 MB) and cut VRAM/upload.

**Normal maps preserve surface shading, NOT silhouette.** A baked normal can't restore a thin
curled petal edge or a stamen that decimation deleted — those are outline geometry. So for
organic silhouettes, decimate **moderately** (start ~40–50%, check the outline by eye) unless you
bake normals, in which case you can push to ~20–30% with the map holding interior detail.

Measured payoff (moderate, no normal bake): 60% of tris + 2K textures = **~5ms p50 / ~7ms p95**.
The aggressive cut with a normal bake is where the bigger geometry win lives, unmeasured here.

## Levers that look real but aren't (measured dead ends)

- **Shadow map resolution** — 4096 → 1024 saved ~0ms. The shadow pass is geometry-bound
  (re-draws all meshes from the light's view); fewer texels, same triangles. Turning shadows
  *off* saves ~4ms/scene, but that changes the look — resolution is free ugliness.
- **Output resolution** — 4K → 1440p did NOT help (geometry-bound, not fill-bound). Only touch
  resolution if a profile shows a fill-bound pass dominating.
- **Dropping the environment/IBL pass** — `bake_environment` is ~1.9ms total across three scenes
  (baked once to an env map, not per-frame). Not worth the lost fill light and reflections.

## Already-shipped engine wins (don't re-derive)

Static-scene shadow caching and IBL gating landed 2026-07-17 (BUG-189 + BUG-197,
`RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md` P0–P5) — shadow maps and env maps for a static scene
no longer re-render every frame. The named next engine lever is **indexed-mesh rendering** (R4,
deferred there): if `render_scene` isn't using the index buffer it processes ~3× the vertices.

## Editing a `.manifold` safely (mechanics)

- The archive is a ZIP of `manifest.json` + `project.json` + `history/`. To change only
  `project.json` while preserving the undo history, `cp` the archive then
  `zip <copy> project.json` from a stage dir — a full unzip/re-zip **drops history** (its files
  extract with broken perms and repackage empty).
- Repointing meshes to lighter versions = string-replace the GLB path in `project.json` (it
  appears in `stringParams`/`stringBindings` defaults and per-node params; ~18 refs/scene).
- Validate every edit through the real loader: `project_tool info <file.manifold>`.
