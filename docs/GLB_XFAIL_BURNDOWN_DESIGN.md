# GLB Xfail Burn-Down — fix every defect-class conformance failure (BUG-164–172)

**Status:** APPROVED design, not built · 2026-07-16 · Fable 5 (Peter in the room)
**Prerequisites:** GLB_CONFORMANCE_DESIGN.md SHIPPED (it is — 2026-07-15); the conformance manifest + `tests/glb_conformance.rs` are the acceptance harness.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's directive (2026-07-16): *"let's get a design doc for all of the bug fixes so we can get all 148 conformance assets passing."* Scope honesty, agreed in the same session: this doc converts the **13 defect-class xfails** (BUG-164–172), taking certification from 56 → 69 of 148. The other 79 xfails are: 49 unsupported material extensions (own doc: `GLTF_MATERIAL_EXTENSIONS_DESIGN.md`), 29 assets never fetched (manifest scope, not code), 1 by-design rejection (BUG-173). Every bug here was re-reproduced on `origin/main` @ `8d9a57b8` on 2026-07-16 before this doc was written.

Instrument frame: these are the "why won't this load" and "why is it black" moments during content-making week. The user-visible contract after this doc ships: **a spec-legal static glb either renders correctly or fails with a named, actionable reason — never silently black, never rejected for something we support.**

## 1. Audit — what exists (verified 2026-07-16, `origin/main` @ `8d9a57b8`)

| Piece | Where | State |
|---|---|---|
| Import entry: `gltf::import(path)` convenience call | `crates/manifold-renderer/src/node_graph/gltf_load.rs:276,337,632` | 3 call sites, all path-based, all subject to the crate's `extensionsRequired` validation veto |
| Default-scene requirement | `gltf_load.rs:281,289,633` | 3 sites `document.default_scene().ok_or_else(…)` — hard error when absent |
| Geometry summary walk | `gltf_load.rs:587-625` (`summarize_node`) | Keys per-material vertex counts by `material().index()`; `None` (default material) counted separately |
| Default-material geometry | `gltf_load.rs:573-582` (`GltfImportSummary.default_material_vertex_count`) | Reported, **explicitly not imported** ("v1 does not import these") — materials list at `gltf_load.rs:656-667` filters on `m.index()?` |
| Graph assembly | `gltf_import.rs:359` (`assemble_import_graph`) | Per-material `render_scene` object wiring; errors "no materials with geometry" at `gltf_import.rs:385` |
| Texture-less material factors | `gltf_import.rs:692-698` | `color_r/g/b` **are** wired from `baseColorFactor` — BUG-169's black render is NOT a missing-factor wire; cause unknown |
| Material map sampler | `render_scene.rs:817-824` | ONE shared `material_sampler`, hardcoded REPEAT on u/v/w (landed `85b5bb9d`, deliberately — fixed the striped-helmet smear) |
| Convergence loop | `bin/render_import.rs:220-291` | byte-stability + `io_pending` + non-black floor; **`last_fraction` is only computed after a stable streak** (`render_import.rs:270-272`), so a reported `0.0000` is ambiguous: "renders black" vs "never went stable" |
| gltf crate | `crates/manifold-renderer/Cargo.toml:47-55` | `gltf 1.4.1`, feature flags per extension; raw-JSON sniff precedent for extensions the crate lacks typed support for (clearcoat, G-P5) |
| Acceptance harness | `tests/glb_conformance.rs` + `tests/fixtures/gltf/khronos/manifest.json` | 148 assets classified; xfails carry named reasons; `scripts/gen_glb_conformance_status.py` regenerates the status doc |

Extend, don't redesign: every fix in this doc lands inside these existing pieces. No new crates, no new threads, no new shared state.

## 2. Decisions

- **D1 — Import entry goes slice-based with our own extension gate (fixes BUG-166).** Replace the three `gltf::import(path)` calls with one shared helper `import_glb(path)` in `gltf_load.rs` that reads the bytes and parses via the crate's lower-level API, so `extensionsRequired` is checked against **MANIFOLD's supported list**, not the crate's feature set. Supported list (initial): everything in `Cargo.toml`'s feature list + `KHR_materials_unlit` + `KHR_materials_pbrSpecularGlossiness` + `KHR_materials_clearcoat` (we map it; the crate has no flag at 1.4.1). Unsupported-but-required extensions still fail loudly, with OUR error naming the extension. ⚠ VERIFY-AT-IMPL: the exact lower-level API shape at gltf 1.4.1 — check `gltf::Gltf::from_slice` vs `Document::from_json_without_validation` + manual buffer/image import (read the crate source in `~/.cargo/registry`, do not synthesize). If 1.4.1's feature flags `KHR_materials_unlit` / `KHR_materials_pbrSpecularGlossiness` alone lift the veto for those two, the feature flags are still not sufficient for clearcoat (verified absent at 1.4.1, `Cargo.toml:42-45`) — the slice-based gate is the root fix; flags alone are the rejected minimal patch. Rejected: enabling feature flags only, because clearcoat-required assets (`ClearCoatCarPaint.glb`) stay vetoed and the next mapped-but-unflagged extension re-opens this bug.
- **D2 — Spec-gloss converts to metal-rough at import (fixes BUG-167).** Parse `KHR_materials_pbrSpecularGlossiness` (enable the crate feature if typed support exists at 1.4.1 — ⚠ VERIFY-AT-IMPL; else raw-JSON sniff per the clearcoat precedent, `Cargo.toml:33`) and convert: `diffuseFactor/diffuseTexture` → base color; `glossinessFactor` → `roughness = 1 - glossiness`; `specularFactor` → `specular_factor` (slot exists since G-P4, `render_scene.rs:263`). `specularGlossinessTexture` maps gloss (A channel) → roughness; its RGB specular tint is **Deferred** (needs a shader change; revival trigger: a real asset where the tint visibly matters). Conversion happens in `gltf_load.rs` material parsing — the graph and shader see only metal-rough. Rejected: a native spec-gloss shading path, because it forks the BRDF for a legacy format the industry converted away from.
- **D3 — Per-map-family samplers replace the single REPEAT sampler (fixes BUG-164).** `render_scene` keeps one sampler **per map family per object** (base/normal/mr/occlusion/emissive — 5 bindings replacing 1), each built from the glTF texture's own `wrapS/wrapT` + min/mag filter, cached by descriptor (samplers are trivially cheap Metal objects). glTF wrap fields ride `GltfMaterialInfo` per map. Default when a texture has no sampler: REPEAT + linear, which preserves today's behavior for every currently-passing asset. Rejected: one sampler per object keyed off base-color, because `TextureSettingsTest` exists precisely to catch per-texture divergence. Consequences, stated honestly: 4 extra sampler bind slots per draw and a wider `GltfMaterialInfo`; both trivial, but golden PNGs for currently-passing assets must be re-verified byte-stable since binding order changes.
- **D4 — Materialless primitives get the glTF default material (fixes BUG-171).** The materials list (`gltf_load.rs:656-667`) gains a synthetic entry for `material().index() == None` geometry when `default_material_vertex_count > 0`: glTF default material per spec (base color [1,1,1,1], metallic 1.0, roughness 1.0). Vertex colors already multiply through the existing path — ⚠ VERIFY-AT-IMPL: confirm `flatten_primitive` (`gltf_load.rs:159`) reads COLOR_0; if not, wiring it is in-scope for the same phase (it's the whole point of `BoxVertexColors`).
- **D5 — No default scene falls back to the union of all scenes' roots (fixes BUG-172).** All three `default_scene()` sites route through one helper: default scene if present; else all nodes of all `scenes[]`; else all parentless nodes. Spec-conformant (the spec says a missing `scene` means "display nothing" is *allowed* but importing is the useful reading — Khronos ships `RecursiveSkeletons.glb` this way deliberately).
- **D6 — Instancing imports as N wired copies, bounded by OBJECT_SAFETY_MAX (fixes BUG-168).** Parse `EXT_mesh_gpu_instancing` (raw-JSON sniff; no typed support at 1.4.1 — ⚠ VERIFY-AT-IMPL) and expand instances into per-object entries at summary time, so the existing 1:1 object pipeline and D4-of-GLB_CONFORMANCE's safety bound apply unchanged. Rejected: true GPU instancing through `render_instanced_3d_mesh`, because it forks the import graph shape for the one Khronos fixture; revival trigger (Deferred): a real asset whose instance count materially exceeds the object budget.
- **D7 — BUG-165/169 are diagnosis-first phases, not pre-decided fixes.** Root causes are unknown; the doc pre-commits the *instrument*, not the theory. First deliverable in each: make `render-import` print the non-black fraction and `io_pending` **every frame** (killing the `last_fraction` ambiguity, `render_import.rs:270-272`), then bisect per the phase brief. Fix follows at the level the cause lives at.
- **D8 — BUG-170 gets a crate-bump pre-flight, then defers to the animation doc.** `cargo update -p gltf --dry-run` + changelog read; if a newer 1.x parses `KHR_animation_pointer` channel targets, take the bump (full conformance suite is the regression gate). If not: the three assets move to the animation doc's scope (`GLTF_ANIMATION_DESIGN.md` owns pointer-targeted animation), and their xfail reason is re-worded to say so. Rejected: pre-parse JSON surgery to strip `animations`, because a loader that silently deletes data it can't parse is the forbidden move of load paths (DESIGN_DOC_STANDARD §5 round-trip corollary).

## 3. Data-model deltas

All in `gltf_load.rs`, content-thread parse time, no serialization impact (import produces a graph; projects store the graph):

- `GltfMaterialInfo` gains per-map-family wrap/filter fields (D3) — exact shape: `wrap: [GltfWrapMode; 5]` or five named fields, executor's choice; the *seam* is that `gltf_import.rs`'s wiring loop reads them per family.
- `GltfImportSummary.materials` may contain the synthetic default-material entry (D4) with a reserved sentinel `material_index` — pin: `u32::MAX`, and `gltf_import.rs` must treat it as "no glTF material to re-query", sourcing everything from the summary entry.
- New `import_glb(path) -> Result<(Document, Vec<buffer::Data>, Vec<image::Data>), String>` helper (D1) — the ONLY parse entry; the three old call sites become one-line callers. Negative gate proves `gltf::import(` is gone.

## 4. Invariants & enforcement

- **Every spec-legal static Khronos asset in the manifest either passes or carries a named xfail reason.** Enforcement: `tests/glb_conformance.rs` — the manifest is exhaustive over fetched assets; an unclassified result fails the suite.
- **No silent geometry drop:** any primitive counted by the summary walk is either imported or named in `ImportReport.report_lines`. Enforcement: new unit test in `gltf_import.rs` tests — a hand-rolled glb with one materialless primitive must yield `object_count == 1` (D4 kills the last drop path).
- **One parse entry:** enforcement: negative `rg 'gltf::import\(' crates/` gate = zero hits outside `import_glb` (P2 deletion gate).
- **Currently-passing assets stay passing:** enforcement: the 56 expect_pass goldens in the conformance suite, re-run at every phase gate.

## 5. Phasing

Worktree note: the conformance fixtures are gitignored; `scripts/agent-worktree.py acquire` copies them. GPU suites run per the CLAUDE.md scope rule (`cargo test`, never nextest, for `--features gpu-proofs`).

### P1 — Diagnosis instrument + BUG-169 + BUG-165 (one session)
- **Entry:** re-run the two repros; both still fail (commands in §6).
- **Read-back:** this doc §2 D7, `render_import.rs:196-291`, BUG_BACKLOG entries 165/169.
- **Deliverables:** per-frame fraction/io_pending trace in `render-import` (a `--trace` flag, default off); root-cause diagnosis written into BUG_BACKLOG for both; the fixes, at whatever level the causes live (escalate per §4 of the standard if a cause crosses a crate boundary the doc doesn't).
- **Gate:** `render-import` on `MetalRoughSpheresNoTextures.glb` and `BoomBox.glb` converges with non-black fraction > 0.02 each; manifest entries flip to `expect_pass` with goldens; full conformance suite green; held-out input: `WaterBottle.glb` (passing today) byte-stable against its golden.
- **Forbidden moves:** raising `frames_max` or lowering the non-black floor to make the gate pass (that's tuning the oracle, not fixing the bug); marking either asset xfail-with-new-reason.

### P2 — Parse-layer trio: D1 slice import + D5 scene fallback + D8 crate pre-flight (one session)
- **Entry:** `rg -c 'gltf::import\(' crates/manifold-renderer/src` returns 3; repros for 166/170/172 still fail.
- **Deliverables:** `import_glb` helper + 3 call-site migrations (compiler-driven: delete the old calls first); scene-fallback helper; crate-bump verdict for BUG-170 written into its backlog entry (bump taken, or defer-to-animation-doc executed).
- **Gate:** positive — `UnlitTest.glb`, `RecursiveSkeletons.glb` import (unlit renders via existing unlit-ish path or escalate; recursive-skeletons renders non-black); `ClearCoatCarPaint.glb` passes the parse layer (render correctness belongs to the already-shipped clearcoat mapping). Negative — `rg 'gltf::import\(' crates/` zero hits outside the helper. Full conformance suite green.
- **Forbidden moves:** keeping any old call site "just for tests"; a validation bypass that skips OUR extension gate (the gate is the fix — bypassing all validation reintroduces BUG-166 as its mirror image: assets we truly can't render importing silently broken).

### P3 — Material trio: D2 spec-gloss + D4 default material (one session)
- **Entry:** P2 landed (spec-gloss assets must parse before they can convert).
- **Deliverables:** spec-gloss → metal-rough conversion; synthetic default-material entry + COLOR_0 verification/wiring; unit tests for both conversions (value-level: known spec-gloss JSON → expected roughness/specular numbers).
- **Gate:** `SpecGlossVsMetalRough.glb` (the asset is literally a side-by-side — the two halves must read alike; golden + region check), `BoxVertexColors.glb` renders with visible vertex colors (golden); full suite green. Held-out: `abandoned_warehouse_-_interior_scene.glb` from Peter's fixtures dir imports and renders non-black (the real-world spec-gloss asset from his log, 2026-07-16 — this is the EP-relevant proof).
- **Forbidden moves:** a separate spec-gloss shading path in the shader; dropping the gloss texture because only the factor is easy.

### P4 — D3 per-map samplers + D6 instancing (one session)
- **Entry:** P1–P3 landed (goldens re-baselined if P1 changed any).
- **Deliverables:** per-map-family sampler plumbing (GltfMaterialInfo fields → render_scene bind); descriptor-keyed sampler cache; instancing expansion at summary time; `TextureSettingsTest` + `SimpleInstancing` manifest entries flipped.
- **Gate:** `TextureSettingsTest.glb` golden (this asset shows wrong-vs-right per quadrant — the golden IS the per-texture-wrap proof); `SimpleInstancing.glb` renders N visibly distinct instances (golden + object_count assertion); **all 56+ prior expect_pass goldens byte-stable** (the sampler change touches every textured draw — this is the phase's real risk, stated honestly); GPU parity suite for render_scene (`cargo test -p manifold-renderer --features gpu-proofs`, render_scene-scoped).
- **Forbidden moves:** one global sampler swap (re-breaks the helmet the REPEAT fix fixed); per-texture sampler arrays in the shader (over-engineering — family granularity is what the fixture tests).

### Landing (per GIT_TREE_DISCIPLINE §2)
Batch P1–P4 per the 2–3-phase batching rule (P1+P2, then P3+P4, or all four if sessions run short); each landing reruns `scripts/gen_glb_conformance_status.py`, updates this doc's Status line, the backlog Status lines for every bug it closes, and writes `docs/landings/`. Final landing updates MEMORY.md's glb-conformance pointer (supersession sweep: the "92 named xfail" figure appears there and in `project_glb_conformance_design.md`).

## 6. Repro commands (all verified failing 2026-07-16)

```
cargo run -q -p manifold-renderer --bin render-import -- tests/fixtures/gltf/khronos/<ASSET>.glb --out /tmp/<asset>.png
# BUG-164 TextureSettingsTest · BUG-165 BoomBox (fetch per manifest pin) · BUG-166 UnlitTest
# BUG-167 SpecGlossVsMetalRough · BUG-168 SimpleInstancing · BUG-169 MetalRoughSpheresNoTextures
# BUG-170 AnimatedColorsCube · BUG-171 BoxVertexColors · BUG-172 RecursiveSkeletons
```

## 7. Decided — do not reopen
1. Slice-based import with MANIFOLD's own extension gate, not feature flags alone (D1).
2. Spec-gloss converts to metal-rough; no native spec-gloss BRDF (D2).
3. Sampler granularity is per map family, not per object, not per texture-array (D3).
4. Instancing expands to objects in v1; no instanced-draw path (D6).
5. BUG-165/169 phases are diagnosis-first; no fix theory is pre-committed (D7).
6. No JSON surgery on unparseable animation data (D8).
7. BUG-173 stays by-design; `OBJECT_SAFETY_MAX` does not move.

## 8. Deferred
- Spec-gloss specular RGB tint → trigger: a real asset where tint visibly diverges (D2).
- True GPU instanced rendering for imports → trigger: instance counts beyond the object budget on a real asset (D6).
- BUG-170's three assets if the crate bump fails → owned by `GLTF_ANIMATION_DESIGN.md` (D8).
- The 29 unfetched glTF-variant assets → manifest-scope decision for Peter; fetching them is a session of `graph_tool`-free plumbing, not a design.
- The 49 material-extension xfails → `GLTF_MATERIAL_EXTENSIONS_DESIGN.md`.
