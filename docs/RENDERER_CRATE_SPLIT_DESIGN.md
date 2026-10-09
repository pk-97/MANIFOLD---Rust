# Renderer Crate Split — one engine crate, node families as leaves

**Status:** IN PROGRESS · Tier 1 and post-T1 production cleanup shipped · P1 boundary landed; baseline measured · P5 implementation underway, extraction not landed. Section 5 (Phasing).
**Prerequisites:** none.
**Work items:** epic BUG-hkbdp (renderer crate split epic); phases BUG-jo1qt (P0 census and seams), BUG-k452g (P1a ui-paint), BUG-9hndn (P1 carve manifold-node-engine), BUG-vnbdt (P2 leaves), BUG-uones (P3 catalog), BUG-l6ltu (P4 review and measurement), BUG-t2jwg (P5 water seam). Status is recorded only above.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before any phase. Lead: Opus 5.5. Lanes: Astra (Codex) for every mechanical phase (Peter, 2026-10-07: *"please use Astra agents for this work"*); this overrides `feedback_astra_review_only` for this campaign only. Lanes make one commit then stop; the lead lands.

<!-- index: Split manifold-renderer into an engine hub, node-family leaf crates, a catalog, the compositor and UI paint; pure moves proven by identity, census and compiler. -->

**The governing insight: the graph engine and its node families need separate compile units.** In the pre-split baseline, `manifold-renderer` held the engine and 455 nodes; a one-line node edit rebuilt 488k lines and a 3,400-test binary. The nodes already register through `inventory`, so they can live in any crate the final binary links. The engine never has to know. What stops this being a file move is that the engine's front door grew family logic inline: physics sources, the scene viewport session, scene-modifier expansion, eight family-specific load migrations. Those are named below with counts; the water family cannot leave until that seam is cut, and the cut is design work, not a move.

Peter's directives, verbatim (2026-10-07): *"ideally we don't place any stop gaps here and use this as an oppurtunity to unify, upgrade, optimise, and improve our rendering creates, boundaries, APIs, and systems to be more professional, higher quality, safer, more stable, and easier to work with."* And on cost: *"Why is it months if it's just a pure refactor and logically can be proven to be the same via the compiler?"* — the answer is D9: the moves are a week; the runtime seam is the second week and it is judgment work.

Stage translation: nothing here changes a pixel or a millisecond. It changes how fast a fix reaches the stage: an edit to a water node rebuilds the water crate's tests, not everything; a lane in the image family cannot break the 3D build; the app can no longer reach into graph execution internals, so a class of "the panel poked the engine" bugs stops being possible.

Binding constraints (DESIGN_AUTHORING.md section 1 (The intake)): *Hot path* — none; no function body changes in any move phase; the one seam phase (P5) touches `PresetRuntime` hooks and gates on the content-thread trace. *Persistence* — none; `EffectGraphDef` and every serialized type stay in `manifold-core`; load migrations keep their order (D5). *Thread residency* — untouched.

Companion docs: `PHYSICS_ENGINE_BOUNDARY_DESIGN.md` (owns the physics graph-adapter boundary this design's P5 depends on, its section 4 (Graph boundary); its G1b `manifold-physics-gpu` sits below the water crate, not instead of it), `RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md` (Wave 3 — file-level splits, the move gate, the WGSL byte-snapshot test), `MODEL_COMMAND_DECOMPOSITION_DESIGN.md` (Wave 2 — the census pattern), `FREEZE_COMPILER_MAP.md` (authority for freeze; its section 2 (File map) gets path updates per landing), `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

---

## 1. Audit — pre-split baseline (verified 2026-10-07)

| Piece | Where | State |
|---|---|---|
| Crate size | `crates/manifold-renderer`: 487,842 lines of Rust, 53% of the workspace (next: app 120,894). `node_graph/` 361k; `node_graph/primitives/` 199k in 455 flat files; `preset_runtime/` 25k; root files 36k. 3,406 `#[test]` in `src/`; 45 integration test binaries (67k lines) in `tests/` | SPLIT |
| Rebuild cost after touching one primitive (`primitives/vignette.rs`, warm target, `CARGO_BUILD_JOBS=4`, measured this session) | lib 5.1s · lib test binary 57.7s (255 CPU-s) · app 38.1s | Historical pre-T1 measurement. Peter skipped the P4 after-measurement; the later post-T1 baseline below records its own method and cache configuration |
| Node registration | `node_graph/primitive.rs:1276` `macro_rules! primitive` — 91 `$crate::` paths, zero bare `crate::` inside the macro body; expands to `inventory::submit!` (`:1408`, `:1421`). `persistence.rs:216` `register_builtin` iterates `inventory::iter::<PrimitiveFactory>`; `descriptor.rs:206`, `param_doc.rs:33` collect the same way | EXISTS — cross-crate registration needs no engine change |
| Other `inventory::iter` consumers | `catalog_gen.rs:181,755`, `validation.rs:2226`, `palette.rs:99,131`, `ports.rs:754` | Work unchanged as long as the family crates are linked (D6) |
| Engine core → primitives, non-test | `rg 'primitives::' node_graph/{execution,graph_loader,validation,effect_node}.rs node_graph/freeze` outside tests: `wgsl_compute` (freeze/install.rs:854), `render_scene::rt_proof::RtProbeScene` (effect_node.rs:1386, `cfg(feature="gpu-proofs")`), `liquid_frame::{WHITEWATER_INPUTS,WHITEWATER_OUTPUTS}` (graph_loader.rs:805), `liquid_stats`/`gpu_flip_preset` re-exports (node_graph/mod.rs:102–103) | The cheap seams — P0 cuts them |
| Engine front door → families, non-test (pre-split seam census, retired after Tier 1; its family table encoded D2 and D11) | 230 sites / 109 edges. By target: `scene_modifier_expand` 61, `physics` 19, `scene_viewport` 18, `fluid` 12, `physics_events` 9, `liquid` 7, `fluid_role` 7, `generators` 7, `render_scene` 4, primitive items (`Gain`, `Mix`, `Blur`, …) 12. By source: `preset_runtime` 66, `metal_backend` 36, `primitive.rs` 25, `execution` 22, `graph_loader` 19. Counted separately because D2 keeps them in the hub: scene **vocabulary** reaches (`depth_rule` 17, `live_extent` 11, `material` 11, `camera` 8, `light`/`transform`/`mesh_source`/`scene_object` 7 each) and `layer_skin` 9 | physics/fluid/liquid adapters and the viewport session are the P5 seam (D9); the rest are P0's cheap cuts (D10) |
| Transitive closure of the engine core (session prototype `closure.py`, same re-derivation) | 177 units, 253k lines including tests: core 60k + scene vocabulary + `scene_modifier_expand` 13.7k + `fluid`/`liquid`/`physics`/`matter` 32k + every water primitive (pulled by `node_graph/liquid/`, 46 `primitives::` refs) + `render_scene` 13.5k (pulled by `gltf_import`, family→family) + `generators/` shared helpers 2.8k. Outside the closure: 371 primitive files 135k, 16 node_graph units 9k, 24 root units 15k | The closure IS the v1 hub minus the cheap cuts; the outside IS the v1 leaves |
| Load-time family migrations in the engine | `graph_loader.rs`: `migrate_gltf_anim_v2` :330, `migrate_gltf_ao_mask` :693, `wire_liquid_intervals` :702, `wire_liquid_frame_cursor` :775, `wire_retained_whitewater` :804, `wire_gpu_flip_grid` :846, `wire_blob_bounds` :1000, `retire_params` :1065 | Eight family passes in the hub — D5 |
| Runtime family glue | `preset_runtime/`: `physics_*` 13 production modules + 10 test modules, `gpu_flip_surface.rs`, `scene_impulses.rs`, `scene_viewport.rs` (89), `math_view.rs` (562) + `math_view_events.rs` (282), `modifier_preview.rs`, `modifier_runtime.rs`. `physics_*` names no primitive (`rg 'primitives::' preset_runtime/physics*` → 0); it depends on `manifold-physics`/`manifold-fluids` types and `node_graph::{physics,fluid}` adapters | Hub-resident in v1 by D2; P5 decides what leaves |
| UI coupling | 8 files import `manifold_ui` (`ui_renderer`, `native_text`, `clip_draw`, `clip_thumb_gpu`, `ui_cache_manager`, `layer_bitmap_gpu`, `automation_lane_draw`, `clip_content_gpu`); none reference `node_graph` (`rg -c node_graph` → 0 for all eight) | A clean leaf with no engine dependency — `manifold-ui-paint`, P1a |
| DAW compositing | `layer_compositor.rs` 5,178 (42 `manifold_core::{project,layer}` refs), `compositor.rs`, `generator_renderer.rs` 2,676, `presentation.rs`, `display_capture.rs`, `preset_thumbnail.rs`, readback, upscalers, tonemap, pq, denoiser. `layer_skin.rs` is referenced 9× from the hub → stays hub | `manifold-compositor`, P2c |
| App surface | 409 `manifold_renderer::node_graph` references in `manifold-app`; top: `bundled_preset_def` 57, `scene_exposure::metadata_for_node_type` 37, `scene_vm::*` ~60, `gltf_import::assemble_import_graph` 15, `PrimitiveRegistry::with_builtin` 9, `freeze::install::pump_segment_results` 5 | Repointed per landing to the owning crate; no re-export facade (D4) |
| Shared node-authoring helpers | `src/generators/`: `mesh_common` (`MeshVertex` 67 primitive refs, `InstanceTransform` 18), `compute_common::Particle` 32, `clip_trigger`, `platonic_geometry`, `line_pipeline`, `mesh_pipeline`, `registry.rs` (legacy generator registry), `bundled_generator_presets.rs` | Helpers move INTO the hub as `manifold_node_engine::{mesh,particles,…}`; the legacy registry and bundled-generator loader go to the catalog (D3) |
| Assets and build script | `preset_loader.rs:369,473` bakes `CARGO_MANIFEST_DIR/assets/…`; `assets/` 5.6 MB (effect/generator/scene-modifier/reference presets, fonts, thumbnails); `build.rs` hashes 16 physics-integration source paths into `MANIFOLD_PHYSICS_INTEGRATION_IDENTITY` | Assets and the preset loader's dev-path resolution move with the catalog crate; `build.rs` path list follows its files (P1 deliverable) |
| Features | `gpu-proofs = ["manifold-gpu/gpu-proofs"]`, `rt-perf-proofs`, `fluid-perf-proofs`, `matter-perf-proofs`, `water-race-probes`, `whitewater-oracle`; `tests/gpu_proofs/main.rs` is one binary of 112 modules | Each feature moves to the crate that owns its tests; the catalog and app forward them (D7) |
| GPU test device | `lib.rs:93` `pub(crate) fn test_device()` RAII shared device; 598 `crate::test_device` uses across 181 files (2026-10-07) | Becomes `manifold_node_engine::testkit` behind a `testkit` feature (D7) |
| Pure-move gate | `scripts/move_identity_check.py` (self-tested; allows `mod`/`use`/`pub use` wiring, `//!`, `#[path]`, `#[cfg(test)]` + test-mod headers, visibility-widening pairs) | EXISTS. Does not know Cargo.toml or new-crate skeletons — P0 extends the allowlist with `Cargo.toml`/`lib.rs` skeleton files named per slice |
| Byte-identity oracles | `freeze/markers.rs:538` `fused_wgsl_snapshot_unchanged` + `freeze/reference.rs` goldens (CPU, seconds); `bin/check_presets.rs` (loads every bundled preset through the real pipeline); `bin/gen_node_catalog.rs` (`dev.py` verb, prints `DRIFT` when the registry differs from the node catalog doc) | EXISTS — the census gates of section 4 (Invariants & enforcement) |
| Regrowth guard | `crates/manifold-app/tests/godfile_regrowth.rs` `CEILINGS` by workspace path | Rows re-pathed per landing (INV-6) |
| Path-keyed tooling | `scripts/*.py` 20 files name `manifold-renderer` (test_landing_gate 34, feature_matrix 8, dev.py 8, landing_gate 6, cpu_scope 6, gpu_scope 4, gpu_proofs_gate 2…); `.config/nextest.toml` 16; `docs/*.md` 101 files name `crates/manifold-renderer/src`; memory 5 files | The sweep inventory, re-derived per phase: `rg -l 'manifold[-_]renderer' scripts .config .claude docs` |
| Concurrent work (2026-10-07 11:00 AEDT) | slot-7 `feat/godfile-trim`: uncommitted edits in `freeze/codegen/{entry_points,mod,types}.rs`, `preset_runtime/mod.rs`, `preset_runtime/tests/`. slot-8 `feat/nightly-gate`: scripts only. `feat/test-warmup`: landed. Nobody in `primitives/` | P1 waits for slot-7; P1a and P0 do not |
| Precedents | `manifold-physics`, `manifold-fluids`: engine crates already extracted, renderer depends on them (the direction this design extends); `manifold-foundation` (UI-reachable shared types); Wave 1–3 pure-move landings | Shape every new crate like these |

Post-T1 baseline (2026-10-09, `b3e492e63`): a warm local-variable rename in
`crates/manifold-nodes-image/src/node_graph/primitives/vignette.rs` took **3.6s**
to rebuild the image library, **13.2s** to build its library test binary, and
**9.3s** to rebuild the app. Each configuration was warmed first; the commands
ran serially with `CARGO_BUILD_JOBS=4`, `CARGO_INCREMENTAL=0`, configured sccache,
and default features. In order: `cargo build -p manifold-nodes-image --lib`,
`cargo test -p manifold-nodes-image --lib vignette --no-run`, and
`cargo build -p manifold-app --bin manifold`. The test command only compiled.
The source was restored byte-for-byte afterward. Initial warmups took
59.5s / 45.0s / 51.0s and are excluded from the edit timings. These shared-Mac
measurements establish a current baseline, not an isolated speedup against the
historical touch-only measurement. The local record and full command arguments
are in `~/.cache/manifold/post-t1-baseline/warm-edit.json`.

The same cleanup's landing passed all 48 checks in about **39 minutes**, with
**zero GPU queue wait**. Test builds took 243s and proof builds 214.5s; selected
default tests took 23.2s. GPU proof execution took 1467.8s, including 30 liquid
proofs at 992.4s. The required broad selection followed shared engine and Metal
backend changes; this is not the expected cost of an ordinary leaf-node edit.
The existing landing timing log records the individual legs under
`codex/wgsl-metadata-ownership` at `2026-10-09T12:17:32Z`.

Classification: **exists** — registration, census oracles, move gate, byte snapshot, device lock, crate precedents. **One wire away** — the testkit feature, the catalog crate's link lines, the layering test. **Genuinely new** — zero runtime systems. D5's migration registry reuses the `inventory` pattern that already serves four registries; it is a fifth row, not a new mechanism. Zero-new-systems test: passes.

Negative claims, checked: no `#![feature]` or `extern crate` tricks keep primitives linked today (`rg 'extern crate' crates/manifold-renderer/src` → 0; everything links because it is one crate). No crate outside `manifold-renderer` submits a `PrimitiveFactory` (`rg -l 'PrimitiveFactory' crates --glob '!manifold-renderer/**'` → 0). `manifold-core`, `-gpu`, `-playback` mention the renderer only in comments; `-recording` and `-spectral` only in feature comments.

---

## 2. Decisions

**D1 — Seven crates replace one; `manifold-renderer` is deleted.** End state and dependency direction (arrows point at dependencies):

| Crate | Holds | Depends on |
|---|---|---|
| `manifold-node-engine` | The engine: `primitive!` + trait, ports, params, bindings, descriptor, persistence/registry, graph, loader, validation, execution, execution plan, freeze/codegen, substeps, state store, snapshot, backend + metal backend, resource allocation, channel names, atomic/composites, `preset_runtime`, `preset_context`, `preset_loader` (minus asset paths), `gpu_encoder`, `render_target(_pool)`, `uniform_arena`, `gpu_types`, `effect`/`effects`, `chain_dispatch`, `layer_skin`, `frame_status`, `background_worker`, `plugin_prewarm`, shared node helpers from `generators/` (as `mesh`, `particles`, `line`, `platonic`, `clip_trigger`), the scene vocabulary modules (D2), and — **in v1 only** — the physics/fluid/liquid graph adapters and the water primitives they name (D9) | foundation, core, gpu, native, playback, physics, fluids |
| `manifold-nodes-image` | Every primitive outside the closure that is not scene or water: 2D effects, generators, color, noise, text, trigger, audio, blob/flow, `text_rasterizer` | graph, gpu, core, image, tiff |
| `manifold-nodes-scene` | `render_scene/`, `render_mesh_diagram`, raytrace, `pbr_material`, `unlit_material`, gltf nodes, mesh nodes, camera/light nodes, `gltf_import/`, `gltf_load`, `gltf_anim_*`, `scene_modifier_legacy_migration/`, `scene_modifier_authoring`, `scene_vm`, `scene_exposure`, `viewport_*`, `material_inspector`, `relight`, `decode_cache` | graph, gpu, core, native, gltf, image |
| `manifold-nodes-water` | P5: the water primitives, `node_graph/{liquid,fluid,fluid_cache,fluid_role,fluid_particles,whitewater*,matter,physics*}`, `preset_runtime/physics_*`, `gpu_flip_surface` | graph, physics, fluids, gpu, core |
| `manifold-nodes` | The catalog: depends on every family so the registry is complete; bundled preset assets + `bundled_presets` loader + legacy generator registry; the bins (`graph-tool`, `check-presets`, `gen-node-catalog`, `freeze-profile`, `render-*`, `generate-preset-thumbnails`); cross-family integration tests folded into one `tests/main.rs` | graph, nodes-image, nodes-scene, nodes-water |
| `manifold-compositor` | `layer_compositor`, `compositor`, `generator_renderer`, `presentation`, `display_capture`, `preset_thumbnail`, `headless_readback`, `gpu_readback`, `metalfx_*`, `fsr1`, `tonemap`, `pq_encoder`, `denoiser`, `live_sim_clock_reference` | graph, core, gpu, playback; dev-dep nodes |
| `manifold-ui-paint` | the eight `manifold_ui` importers | gpu, ui, foundation, core |
| `manifold-app` | unchanged role | all of the above |

Rejected: *two crates (engine + everything else)* — the compile win needs leaves, and families churn independently; one "everything else" crate is the current problem with a new name. Rejected: *one crate per primitive* — 455 crates is Cargo overhead and a registry per crate; families are the natural unit because cross-primitive imports stay inside them (`sort_particles_into_cells`, `prefix_scan`, `gpu_flip_step` are all water-internal). Rejected: *a `manifold-engine` crate that re-exports graph + families* — that is the facade D4 forbids.

**D2 — The scene vocabulary is engine, not family.** `camera`, `light`, `material`, `transform`, `atmosphere`, `render_mode`, `live_extent`, `mesh_source`, `source_asset`, `scene_object`, `depth_rule`, `mesh_change`, `boundary_nodes`, `mesh_boundary`, `mesh_partition`, `vector_field`, `physics_mesh` are the typed port and param shapes the execution core, backend and freeze speak (`metal_backend` 36 sites, `primitive.rs` 25). They move to `manifold-node-engine::scene`. Consequences, stated honestly: the engine knows what a camera and a material *are* without owning any renderer for them; that is the same relationship `manifold-core` has to clips. `scene_modifier_expand` (13.7k, 61 sites from the loader and `bound_graph`) is load-time graph expansion driven by tables; it stays in the hub as `manifold_node_engine::expand` in v1 and is a named P5 candidate. Rejected: *a separate `manifold-scene-types` crate* — nothing but the hub and the scene family would use it, and the hub needs all of it.

**D3 — Shared node helpers leave `generators/` for the hub, renamed by what they are.** `mesh_common` → `manifold_node_engine::mesh`, `compute_common` → `::particles`, `line_pipeline` → `::line`, `mesh_pipeline` → `::mesh::pipeline`, `platonic_geometry` → `::platonic`, `clip_trigger` → `::clip_trigger`, `stateful_base` → `::stateful`. The 154 `crate::generators::` reaches from primitives become `manifold_node_engine::mesh::MeshVertex` etc. — path edits only, allowed residue. `generators/registry.rs` and `bundled_generator_presets.rs` (the legacy generator table and its JSON loader) go to the catalog. Rejected: *keep a `generators` module name* — it is the Unity-era name for a dead concept (CLAUDE.md: every generator is a graph now) and the hub should not carry it.

**D4 — No facade, no re-exports, no transitional shims.** Each landing moves items and repoints every importer in the same commit. `manifold-renderer` shrinks by exactly what left until P3 deletes it. Peter: *"we don't place any stop gaps here."* What a sequence of landings necessarily has — a `manifold-renderer` that still holds families while the hub exists — is not a shim: nothing is built to support it, and every landing deletes what it moved. Consequences, stated honestly: the app's 409 import sites get touched across P1–P3 instead of once; that is the cost of never having a crate that lies about where things live. Rejected: *`pub use manifold_node_engine::*` in `manifold-renderer` during the transition* — hides the real surface, survives the campaign by inertia (Wave 2's `mod.rs` facades are still the public surface there), and makes INV-2's census blind.

**D5 — Family migrations register; the loader runs them in two fixed stages around the group flatten.** New in `manifold-node-engine::load::migration`:

```rust
pub enum MigrationStage {
    /// Needs the still-nested group structure.
    BeforeFlatten,
    /// Runs on the flat document.
    AfterFlatten,
}

pub struct GraphMigration {
    /// Stable id; breaks ties within an `order` value.
    pub name: &'static str,
    pub stage: MigrationStage,
    /// Lower runs first within the stage. Values are committed below.
    pub order: u16,
    /// Returns true when it changed the def. Must be idempotent.
    pub apply: fn(&mut EffectGraphDef) -> bool,
}
inventory::collect!(GraphMigration);
```

The committed sequence reproduces `instantiate_def` as read by the lead on 2026-10-07. Hub and core steps stay direct calls, in this order: phong-to-PBR (core), `retire_params` (hub), scene-modifier preparation, `migrate_def_type_ids` (needs the registry), scene-object rewiring (core). Then the registered `BeforeFlatten` stage: 200 `migrate_gltf_anim_v2`, 210 `migrate_gltf_ao_mask`. Then `flatten_groups`. Then the registered `AfterFlatten` stage: 300 `wire_liquid_intervals`, 310 `wire_gpu_flip_grid`, 320 `wire_liquid_frame_cursor`, 330 `wire_retained_whitewater`, 400 `wire_blob_bounds`. Each registered migration lives in the module that owns its node types, so the P1 and P2 moves carry it without an edit. The walk keeps today's semantics exactly: each migration runs on a fresh clone of the current def, and the clone is kept only when `apply` returns true. A test `migration_order_matches_table` pins the resolved sequence per stage. Enforcement: that test plus the LiveSchool round-trip (`manifold-io/tests/load_project.rs`) at P0 landing. Rejected: *one sorted walk with `retire_params` last* — the loader runs `retire_params` first, before expansion, and the glTF passes need the unflattened groups, so one list cannot reproduce today's order. Rejected: *each family exposes `fn migrate(def)` and the loader calls them* — the hub would name the families, which is INV-1's violation by construction. Rejected: *ordering by declaration* — `inventory` iteration order is link order, not stable.

**D6 — Linking is proven, never assumed.** A node crate nothing references may be dropped by the linker, and every primitive in it silently vanishes from the registry. Three guards: (a) `manifold-nodes/src/lib.rs` holds `pub use manifold_nodes_image as image;` etc. — a real reference per family; (b) every binary that loads presets (app, catalog bins, compositor tests, any `tests/main.rs` that calls `with_builtin`) depends on `manifold-nodes`, not on individual families; (c) the primitive census (INV-3) runs at every landing and fails on count drift. Rejected: *`#[used]`/`ctor` tricks* — `inventory` already does that; the failure mode is an unreferenced rlib, and only a reference fixes it.

**D7 — Test infrastructure splits with the code; one test binary per crate.** The shared GPU test device (`TestDevice`, `test_device()`, the in-process lock) moves to `manifold_gpu::testkit` behind `manifold-gpu`'s `gpu-proofs` feature, next to the machine-wide GPU queue it already takes; P1a moves it, because `manifold-ui-paint` may not depend on the hub even for tests. `manifold-node-engine` exposes `pub mod testkit` under feature `testkit` for graph fixtures (now in `tests/common`, `tests/support`); families enable it via `[dev-dependencies] manifold-node-engine = { path, features = ["testkit"] }`. `gpu-proofs` lives on `manifold-node-engine` (forwarding `manifold-gpu/gpu-proofs`) and each family re-declares `gpu-proofs = ["manifold-node-engine/gpu-proofs", "manifold-node-engine/testkit"]`; the perf and oracle features move to `manifold-nodes-water` (`fluid-perf-proofs`, `matter-perf-proofs`, `water-race-probes`, `whitewater-oracle`) and `manifold-nodes-scene` (`rt-perf-proofs`). `tests/gpu_proofs/main.rs`'s 112 modules go to the crate owning each proof; the 45 plain integration binaries fold into one `tests/main.rs` per owning crate (this is BUG-yd6b (fold per-file test binaries into one per crate), closed by P3). The device lock stays an in-process static, so sequential `cargo test` across crates is as serialized as today. `gpu_scope.py` rows and `gpu_proofs_gate.py` crate lists are re-pathed per landing. Rejected: *a `manifold-testkit` crate* — a dev-dependency cycle (testkit → graph → … → testkit in tests) compiles the hub twice; a feature on the hub costs nothing.

Test-only engine items use `#[cfg(any(test, feature = "testkit"))]`, never plain `#[cfg(test)]`: dependency builds never see `cfg(test)`. Tests whose subject is a real node or bundled preset stay renderer-side in `catalog_tests` or `engine_contract_tests` until P3 folds them into the catalog. A fixture rewrite is valid only when the replaced node is incidental to the test.

**D8 — Visibility widens only on compiler demand and is reviewed once.** A move that needs `pub(crate)` → `pub` makes exactly that change; `move_identity_check.py` already allows widening pairs. Each landing's report lists its widenings. P4 reviews the full list crate by crate: keep (documented, part of the designed surface) or narrow (compiler-driven: narrow, build, fix, repeat). No `pub` is added speculatively; no `pub(crate)` becomes `pub` to "make the API nicer" during a move. Rejected: *design the public surfaces up front* — the compiler's list is the real inventory; designing against a guessed one is the "remembered codebase" failure.

**D9 — Two tiers; the water seam is the second.** Tier 1 (P0–P4) is pure moves plus the cheap cuts, and lands everything in D1 except `manifold-nodes-water`. The hub closure proves why: `node_graph/liquid/` names 46 water primitives, `substeps`, `preset_runtime` and `execution` name `liquid`/`fluid`/`physics` at ~60 sites, and `preset_runtime/physics_*` is runtime scheduling of physics sources. Moving those is not a move — it is the graph-adapter boundary `PHYSICS_ENGINE_BOUNDARY_DESIGN.md` section 4 (Graph boundary) owns, plus a runtime-extension seam for `PresetRuntime`. Tier 2 (P5) is a seam brief authored from P0's compiler-derived inventory, led by Opus with the consult seat, after `PHYSICS_ENGINE_BOUNDARY` P1 lands. Consequences, stated honestly: after Tier 1 the hub is ~250k lines and still carries the water solver nodes; the compile win for water edits arrives with P5, not P3. Peter's publishable-water goal is P5's acceptance, not Tier 1's. Rejected: *cut the seam first, then split* — blocks a week of mechanical wins behind the one judgment-heavy phase, and the seam's inventory is better when the compiler produces it against a real crate boundary (P0). Rejected: *leave the adapters in the hub permanently* — contradicts the physics design's D1 and Peter's engine goal.

**D10 — Cheap seams cut in P0, each one named.** (a) `effect_node.rs:1386` `rt_probe_scene` trait method (gpu-proofs) → deleted; the scene crate's proofs downcast through `Primitive::as_any` (add `fn as_any(&self) -> &dyn Any` to the trait with a default body only if absent — ⚠ VERIFY-AT-IMPL `rg -n 'fn as_any' crates/manifold-node-engine/src/primitive.rs`). (b) `graph_loader.rs:805` → D5. (c) `freeze/install.rs:854` `wgsl_compute::select_compute_entry_name` → `wgsl_compute` is a hub built-in (D11). (d) `node_graph/mod.rs:102–103` re-exports of `gpu_flip_preset`/`liquid_stats` → removed; importers use the owning path. (e) `substeps.rs:437` and `execution_plan.rs:25` reaching `generators::` → D3 paths. (f) `composites/` naming `Gain`, `Mix`, `Blur`, `Brightness`, `ColorRamp`, `Math`, `Value`, `BeatGate`, `FlowFieldNoise` → composites move to `manifold-nodes-image` with the primitives they compose, except any composite the hub's non-test code instantiates (⚠ VERIFY-AT-IMPL `rg -n 'composites::' crates/manifold-node-engine/src/{runtime,exec/execution.rs,load/graph_loader.rs}`), which makes its members built-ins (D11). (g) `preset_runtime/math_view*.rs` naming `RenderMeshDiagram::prewarm_pipelines` and `BeatEnvelope*` → `math_view` moves to `manifold-nodes-scene` as a wrapper over the public `PresetRuntime` API if `preset_runtime/mod.rs` only *calls into* it; if `PresetRuntime` *stores* math-view state, it is a P5 item and P0 records it. (h) `preset_runtime/scene_viewport.rs` (89 lines) + `node_graph/scene_viewport` (18 hub sites, `execution.rs:221`) → P0 reads the four `execution.rs` sites; if they are a viewport-camera *input* the engine samples, the type is vocabulary (D2) and stays; the session logic goes to the scene crate.

Outcomes, decided at P0 (2026-10-07; site detail in `.claude/orchestration/crate-split-seams.md`): (a) `EffectNode` gains the standard blanket `AsAny` supertrait; the RT probe lookup is a gpu-proofs free function beside `RtProbeScene`. (e) went one step further than named: every importer of the mesh and particle helpers already uses `crate::mesh` / `crate::mesh::pipeline` / `crate::particles` (declared by `#[path]` until P1 moves the files), so P1's rewrite is uniform. (f) no hub code instantiates a composite: composites are image-family. (g) `PresetRuntime` stores `math_views`: a P5 item. (h) the executor rendered the viewport through a `RenderScene` it owned. The hub now defines `ViewportPass` (render, texture, status, errors, clear_state); `EffectNode::viewport_pass()` returns a fresh pass with its own renderer and history, `RenderScene` is the only provider, and the executor holds `Box<dyn ViewportPass>`, built once per target change. Whether a node offers a pass is the only test; the hub names no node type. Config, camera and error types stay hub vocabulary.

**D11 — Built-in primitives: the hub may contain a primitive only if hub non-test code names it.** Committed list: `wgsl_compute`, `standalone_pipeline` (a helper, not a node), `Mix`, `MaskedMix`, `MuxTexture`, `Value`, `Gain`. The list is a constant in `manifold-node-engine/src/builtins.rs` with a test asserting the registry's hub-crate factories equal it. Anything else the compiler pulls in is an escalation, never a silent addition. ⚠ VERIFY-AT-IMPL at P0: `rg -o 'primitives::[A-Z]\w+' crates/manifold-node-engine/src/{runtime,exec,load}/*.rs --glob '!*test*' | sort -u`.

**D12 — Layering is a test over `cargo metadata`, not prose.** `crates/manifold-app/tests/crate_layering.rs` reads `cargo metadata --format-version 1 --no-deps`, builds the workspace edge set (normal + build deps; dev-deps separately), and asserts the table in section 3. Precedent: `godfile_regrowth.rs` (a workspace-reading test with a committed table). It lands in P1a with the first new crate and grows a row per phase. `PHYSICS_ENGINE_BOUNDARY` P1's deny.toml rows coexist; this test is the one that names every workspace crate.

**D13 — The engine crate is grouped by job, not flat.** `manifold-node-engine` has a small root (graph, primitive and its macro, ports, parameters, bindings, descriptor, persistence, validation, snapshot, state store, built-ins) and named groups: `exec` (execution, plan, effect node, backends, resource allocation, substeps), `freeze`, `load` (graph loader, `load::migration` per D5, `load::expand` per D2, chain spec, preset loader), `runtime` (`PresetRuntime` and the runtime-root modules), `gpu` (encoder, render targets, uniform arena, GPU context), `scene` (the D2 vocabulary plus `viewport_camera` and `scene_viewport`), the D3 helpers at the root, `primitives` (exactly the D11 list) and, in v1 only, `water` (the D9 adapters, their runtime files and the water primitives). The P1 brief carries the file-by-file list. `gltf_anim_identity.rs` stays scene-family: it depends on scene decoding and contributes through `PhysicsSourceIdentity`, rather than entering the engine. Build scripts hash only their own crate's sources; family contributions cross the boundary through `PhysicsSourceIdentity`. The v1 water closure also owns `face_grid_scenes`, `offset_lattice`, `redistance_lattice`, `clamp_liquid_to_solids`, `smooth_lattice`, `relax_surface_mesh`, `shape_particle_blobs`, `count_surface_edges`, `surface_mesh_parity`, `turbulence_field`, `inside_turbulence_potential`, `turbulence_emission_count`, `dust_potential`, `advect_whitewater`, `age_whitewater`, `retype_whitewater`, `jitter_particles`, `sample_faces_at_particles`, `wavecrest_potential` and `spawn_whitewater`; existing test/proof gating stays intact. Move-list corrections: the six `effects/shaders/{aces_tonemap_compute,fsr1_easu_compute,fsr1_rcas_compute,linear_to_pq_compute,presentation,tonemap_common}.wgsl` files follow compositor; `fx_watercolor_compute.wgsl` follows image; `relax_surface_mesh` follows water. Shader bytes remain unchanged. Consequences: P5 moves `water` as one directory; INV-7 excludes water primitives by path. Rejected: *a flat root* — about 110 top-level modules, the v1 water residue indistinguishable from the engine, and D5's and D2's own paths contradicted. Rejected: *re-nesting later* — every importer would be rewritten twice.

---

## 3. Layering table (enforced by `crate_layering.rs`, D12)

Forbidden edges, normal and build dependencies (dev-deps may cross downward only to `manifold-nodes`):

| Crate | May depend on | Must never depend on |
|---|---|---|
| `manifold-node-engine` | foundation, core, gpu, native, playback, physics, fluids | ui, editing, io, media, app, any `manifold-nodes*`, compositor, ui-paint |
| `manifold-nodes-{image,scene,water}` | graph + graph's allowed set | each other, `manifold-nodes`, ui, editing, io, app, compositor, ui-paint |
| `manifold-nodes` | graph, the three families | ui, editing, io, app, compositor, ui-paint |
| `manifold-compositor` | graph, core, gpu, playback, foundation | any `manifold-nodes*` (dev-dep on `manifold-nodes` allowed), ui, editing, io, app |
| `manifold-ui-paint` | gpu, ui, foundation, core | graph, any nodes, compositor, editing, io, app |

`manifold-node-engine` depending on `manifold-core` is the honest cost of v1: `EffectGraphDef`, `ParamManifest`, `NodeId`, `Seconds` and the effects types live there (226 of node_graph's files import `manifold_core`). Extracting a `manifold-graph-def` crate from core is deferred (section 7) — it is Wave 2 territory with serialization on the line.

---

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| INV-1 The hub names no family | `rg -n 'manifold_nodes|nodes_(image|scene|water)' crates/manifold-node-engine/` → 0, in `crate_layering.rs`; plus the Cargo edge table |
| INV-2 Every move landing is a pure move | Primary proof: `scripts/crate_move_replay.py verify --plan <dir> <commit>` must reproduce the complete Git tree from its immediate parent; the complete plan and tool must be committed and reviewed BEFORE the move, and the move cannot add, change or delete plan files. Plans allow only renames, validated Rust/file path rewrites, module wiring derived from moves with byte-identical visibility and attached attributes (new-parent templates must contain the exact mount), manifests and templates; declarations.tsv and arbitrary source patch rows are forbidden, use items only change through path rewrites, missing/ambiguous/inline/#[path] mounts fail closed, and reviewers sign off on the exact template bytes and manifest hunks identified by verify's SHA-256 review digest. Reproducibility proves the reviewed transformation, not semantic purity: body changes and residual fixes belong in separate reviewed commits, and `scripts/move_identity_check.py --plan <dir> <commit>` is the second check; its unproved residue is reviewed with the phase. |
| INV-3 The registry is complete after every landing | `cargo run -p <crate holding the bins> --bin gen_node_catalog -- --check` prints `node catalog in sync`: `docs/node_catalog.json` lists every registered node with its ports and params and is compared byte for byte (389 nodes, 99 presets at P0), so a dropped family fails it; AND `check-presets` exit 0 (it opens the GPU, so the lead runs it through `gpu_queue.py`) |
| INV-4 Emitted WGSL is byte-identical | `fused_wgsl_snapshot_unchanged` + `freeze/reference.rs` goldens, unmodified, green at every landing that touches `freeze/` or a primitive file (CPU test, seconds) |
| INV-5 No test is lost in a move | `scripts/test_census.py` (P0 deliverable; `cargo nextest list --workspace --message-format json` → multiset of test names with the crate prefix stripped) equal before and after each landing, drift printed by name |
| INV-6 Decomposed files do not regrow across the move | `godfile_regrowth.rs` CEILINGS rows re-pathed in the same landing; the test is green before push |
| INV-7 Built-ins are exactly the D11 list | `builtins_match_registry` in `manifold-node-engine`: the engine crate's registered factories outside `water::primitives` equal the D11 list. |
| INV-8 Load migrations run in the committed order | `migration_order_matches_table` + LiveSchool round-trip |
| INV-9 No per-frame change | Move phases: none needed (bodies unchanged, proven by INV-2). P5: `MANIFOLD_RENDER_TRACE=1` run on the water demo project, no frame > 20 ms |

---

## 5. Phasing

**Work items:** epic BUG-hkbdp (renderer crate split epic); phases BUG-jo1qt (P0 census and seams), BUG-k452g (P1a ui-paint), BUG-9hndn (P1 carve manifold-node-engine), BUG-vnbdt (P2 leaves), BUG-uones (P3 catalog), BUG-l6ltu (P4 review and measurement), BUG-t2jwg (P5 water seam).

**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before any phase. Lead: Opus 5.5. Lanes: Astra (Codex) for every mechanical phase (Peter, 2026-10-07: *"please use Astra agents for this work"*); this overrides `feedback_astra_review_only` for this campaign only. Lanes stop after edits and checks; the lead commits and lands.

Common to every phase: one Astra lane per phase or sub-phase in its own slot worktree (`scripts/agent-worktree.py acquire`); the brief carries this doc's section numbers, the entry commands, the gate; the lane finishes its working-tree change and stops; the lead reviews the diff against INV-2 before anything else. Astra's sandbox cannot write git metadata, so lanes never stage or commit: they move files with plain `mv`, report the file list per intended commit, and the lead makes pathspec commits (git pairs the renames at commit time). Lanes build with `RUSTC_WRAPPER=` empty (sccache cannot run in the sandbox), widen visibility the compiler demands without asking (D8), and record anything the design does not cover as an escalation while finishing the rest. A diff carrying judgment gets a Fable review at medium effort before it lands; a pure move that passes INV-2 does not. Rules of the move: every relocation is a rename in its commit (blame survives; append the landing SHA to `.git-blame-ignore-revs`); a file whose rewritten lines push git below its rename threshold lands as two commits, the move then the rewrite, gated per commit; no body edits in a move commit; seam edits are their own commits, before the move, with their own tests. Lanes may touch `scripts/*.py` only for the path rows the brief names.

### P0 — Census, tooling, cheap seams (serial; lead + one Astra lane; no new crate yet)

- **Entry:** main at or after `7c3de26c5`. `slot-7` state irrelevant (P0 touches none of its files except `graph_loader.rs`, which it does not hold).
- **Read-back:** sections 1, 2 (D5, D10, D11), 4. Restate the D10 list and the three census oracles.
- **Deliverables:** the pre-split closure/seam census (retired after Tier 1; implementation preserved in git) and `scripts/test_census.py` (verb `test-census`); `move_identity_check.py` allowlist extension + self-test cases; `GraphMigration` (D5) with its order test; the D10 cuts (a)–(h), each its own commit with the tests the touched module already has; `builtins.rs` (D11); the findings file `.claude/orchestration/crate-split-seams.md` (D10 outcomes, D11 verification, P5 items). The compiler-derived inventory comes from P1's first stage, not a throwaway carve: doing the carve twice buys nothing.
- **Gate:** `cargo nextest run -p manifold-renderer -E 'test(graph_loader) | test(migration) | test(builtins) | test(freeze::markers)'` green; INV-3, INV-4, INV-5 baselines recorded in the findings file (counts and the exact commands); `scripts/test_dev.py` green (new verbs registered); LiveSchool round-trip green in the main checkout.
- **Demo:** none — L1.
- **Forbidden:** creating the real `manifold-node-engine` crate; moving any file; "while I'm here" edits in `graph_loader.rs` beyond the eight call replacements; widening any visibility.
- **Scope:** one session.

### P1a — `manifold-ui-paint` (parallel with P0; one Astra lane)

- **Entry:** P0 not required. `rg -c node_graph` → 0 for the eight files (re-verify).
- **Deliverables:** `crates/manifold-ui-paint/{Cargo.toml,src/lib.rs}`; `manifold_gpu::testkit` (D7) in a seam commit before the move, with the two UI test modules repointed off hub types; `git mv` of the eight files; app imports repointed; `crate_layering.rs` with the ui-paint row (D12, first landing of the test); nextest filtersets re-pathed.
- **Gate:** INV-2 zero residue; `cargo clippy -p manifold-ui-paint -p manifold-app -- -D warnings`; INV-5; `scripts/run_ui_flows.py --touched` (the paint path is what every flow exercises); `crate_layering` green.
- **Demo:** L3 — the existing flow suite; count must match the manifest.
- **Forbidden:** moving `text_rasterizer.rs` (two primitives use it — it is image-family); any `pub` added beyond compiler demand.

### P1 — Carve `manifold-node-engine` (serial; one Astra lane; after slot-7 lands)

- **Entry:** P0 landed; the findings file.
- **Read-back:** D1 hub row, D2, D3, D7, D8, D11; the findings file.
- **Stages:** stage 1 moves the D1 hub list into `crates/manifold-node-engine`, runs `cargo check -p manifold-node-engine`, writes every unresolved reach into a family to the findings file (file:line, owning D1 crate) and stops; the lead rules on each; stage 2 finishes the carve. Both stages are one commit.
- **Deliverables:** `crates/manifold-node-engine` with the D1 hub contents by `git mv`; `generators/*` helpers renamed per D3; `testkit` feature; `gpu-proofs` feature moved; `build.rs` path list re-pathed; `manifold-renderer` depends on `manifold-node-engine` and keeps the rest; every `crate::node_graph::` path in the moved and remaining files rewritten; app imports for moved items repointed; `crate_layering.rs` hub row; `godfile_regrowth.rs` rows re-pathed; `gpu_scope.py`/`cpu_scope.py`/`nextest.toml`/`gpu_proofs_gate.py`/`feature_matrix.py` rows for hub paths; `FREEZE_COMPILER_MAP.md` section 2 (File map) paths.
- **Gate:** INV-2, INV-3, INV-4, INV-5, INV-6, INV-7, `crate_layering`; `cargo clippy -p manifold-node-engine -p manifold-renderer -p manifold-app -- -D warnings`; `scripts/gpu_proofs_gate.py` in its scoped mode (the freeze and runtime proofs it maps for the touched paths); LiveSchool round-trip.
- **Demo:** L2 — `target/debug/examples/fluid_capture OUT --preset <one water preset>` and one scene preset via `gpu_queue.py`; Peter looks; agents compare the pre-landing PNG with a scripted pixel diff, threshold 0.
- **Forbidden:** resolving a compile error by moving a family module into the hub without an escalation line in the report (the compiler's demand beyond the findings file IS the escalation); re-exports; editing any function body.
- **Scope:** one long session. Expect the single largest diff of the campaign; it is still one commit.

### P2 — Leaves in parallel (three Astra lanes, disjoint files)

- **P2a `manifold-nodes-image`**, **P2b `manifold-nodes-scene`**, **P2c `manifold-compositor`.** Entry: P1 landed. Each: new crate; `git mv` per D1; its `tests/gpu_proofs` modules and plain integration tests folded into `tests/main.rs` and `tests/gpu_proofs/main.rs`; its features (D7); app imports repointed; layering row; regrowth rows; scope-script rows; the family's proofs mapped in `gpu_scope.py`.
- **Gate (each):** INV-2 through INV-6; `cargo clippy -p <crate> -p manifold-renderer -p manifold-app -- -D warnings`; `scripts/gpu_proofs_gate.py` scoped to the moved paths (must select the family's smoke set — a lane whose scope run selects nothing has a mapping gap: stop).
- **Demo:** L2 — one preset render per family through `gpu_queue.py`, pixel-diffed at threshold 0 against the pre-landing PNG.
- **Forbidden:** touching another lane's files; "fixing" a test that moved; resolving an import by adding a dependency between families.
- **Shared-file conflicts:** `crates/manifold-app/Cargo.toml`, `Cargo.toml` (members), `gpu_scope.py`, `nextest.toml`, `godfile_regrowth.rs`. Landing order P2a → P2b → P2c; each later lane merges main before its gate (landing protocol).

### P3 — The catalog, and `manifold-renderer` is gone

- **Entry:** P2a–c landed. `manifold-renderer` now holds: bins, `assets/`, `bundled_presets`, `generators/registry.rs` + `bundled_generator_presets.rs`, cross-family tests. (The water primitives and adapters are in `manifold-node-engine` after P1 per D9, not here.) ⚠ VERIFY-AT-IMPL: `fd -e rs . crates/manifold-renderer/src | wc -l` and list; anything not in this sentence is an escalation.
- **Deliverables:** `crates/manifold-nodes` per D1 and D6; `assets/` moved; the `PresetAssetsRoot` registration moves from `manifold-renderer` to `manifold-nodes` with the assets; bins moved; cross-family tests folded; `crates/manifold-renderer` deleted; workspace members updated; every remaining live `manifold-renderer`/`manifold_renderer` reference in `scripts/`, `.config/`, `.claude/`, `docs/` resolved (re-derive: `rg -l 'manifold[-_]renderer' scripts .config .claude docs crates`) — zero live references is the deletion gate; BUG-yd6b (fold per-file test binaries into one per crate) closed.
- **Gate:** INV-1 through INV-7; full `scripts/landing_gate.py`; `scripts/feature_matrix.py` (every moved feature builds); `rg -l 'manifold[-_]renderer' …` → 0 live references; historical descriptions (including this design and its generated index entry), archived docs, committed replay plans and git history retain the source crate name.
- **Demo:** L3 — full flow suite, count match; plus the two P1 preset renders at threshold 0.

### P4 — Surface review, measurement, docs

- **Entry:** P3 landed.
- **Deliverables:** the widening review (D8) as one commit per crate narrowing what the review rejects; doc headers for each new crate's `lib.rs` (what it is, what it never depends on, in the house voice); the freeze compiler map, the primitive authoring guide, the node catalog paths, the development reference's module layout, CLAUDE.md's crate table, the context-nudge path table, and `scripts/gen_docs_index.py`; the after-measurement (same three timings as the audit, same method, same file touched in each family) written into the audit's rebuild-cost row; this doc's status → `IN PROGRESS · Tier 1 shipped · P5 open`.
- **Gate:** `cargo clippy --workspace -- -D warnings` once (the only workspace sweep in the campaign — multi-crate landing, justified); docs lifecycle check; `design_status.py`.
- **Demo:** none — L1. The measurement is the artifact.

### P5 — The water seam (Tier 2; Opus lead with the consult seat; brief authored from P0's findings)

The P0 closure/seam census assumed the monolithic renderer layout and was retired after Tier 1. Re-derive the current reaches from `manifold-node-engine::{runtime,exec,load,water}`; the historical findings are context, not a current dependency inventory.

- **Entry:** P4 landed; `PHYSICS_ENGINE_BOUNDARY` P1 landed (its transaction and deny rows); the findings file's P5 section (every hub→{liquid, fluid, physics, matter, whitewater, gpu_flip_*} site by file:line, from the compiler).
- **What it decides (not decided here, by design):** the `PresetRuntime` extension seam for physics sources, the `substeps`↔`liquid` clock contract (owned by the live sim clock design — coordinate, don't amend), whether `scene_modifier_expand` follows. Mechanism constraint already fixed: registration through `inventory`, like D5; no new shared state; no trait object constructed per frame. The phase ends with `manifold-nodes-water` existing per D1, the hub free of `physics`/`fluids` dependencies (layering row flips), and INV-9's trace clean.
- **Gate:** every invariant above plus `scripts/gpu_proofs_gate.py` with the water proof set (`fluid-perf-proofs` features on) and `scripts/rt_noise_gate.py` unchanged.
- **Demo:** L2 — Peter's water demo project renders identically (pixel diff at threshold 0 on the three fixed frames the FLIP proofs already capture).

#### P5 preparation seam (verified 2026-10-09 at `d5d226167`)

The current static inventory finds 136 non-water source lines naming water or
native physics in 35 engine files (92 production, 44 test-only). This is a source
inventory, not the compiler-derived closure still required before moving files.
Re-derive with `rg -n 'crate::water|manifold_physics|manifold_fluids'
crates/manifold-node-engine/src --glob '*.rs' --glob '!**/water/**'`.

| Remaining boundary | Current owner / consumer |
|---|---|
| Three CPU wire payloads: `RigidBody`, `FluidRole`, `FieldValue` | `bindings.rs`, `exec/{backend,metal_backend,execution}.rs`; water owns native values and prepared geometry |
| Native observations, impulses and coupled publication | `primitive.rs`, `exec/effect_node.rs`, `graph.rs`, `water/physics_scene.rs` |
| History, source identity, state carry and scene impulses | Seven `PresetRuntime` impl blocks and one `EffectSlot` impl in `water/runtime`; state is embedded in `runtime/core.rs` |
| Clock interval carrier | `exec/substeps.rs::SubstepInterval` embeds the native `StepInterval`; only accepted endpoints and loop position cross this boundary |
| Asset cooking and proof helpers | `scene/physics_mesh.rs::prepare_colliders`, `scene/vector_field.rs::ContinuousField`, built-in texture extent registrations and water test fixtures |

Do not move native physics algorithms or caches into foundation merely to hide
the dependency. Do not add a per-value box on graph writes: these wire values
currently clone without allocating in the steady frame path. The remaining
wire/runtime extension signatures must be pinned before their workers start.

The runtime calls two water transformations directly: `runtime/modifier_runtime.rs`
(`from_def_for_render_view`) prepares the sparse surface before scene expansion;
`runtime/build.rs` (`from_render_def`) wires the flattened FLIP grid before binding
capture. Both are construction-time operations. The grid transformation already
registers with `load/migration.rs::GraphMigration` for ordinary graph loading.

Extend that existing registry with `BeforeSceneModifiers` and
`BeforeBindingCapture` stages. Register `gpu_flip_surface::prepare` at the first
and `liquid::migration::wire_gpu_flip_grid` at the second. The engine calls
`prepare_stage(&mut EffectGraphDef, MigrationStage) -> bool` at the existing call
sites; it runs every callback in `(order, name)` order and reports whether any
changed the owned runtime document. Surface preparation returns that change flag
through its existing recursive walk. No new registry, document clone, runtime
state, per-frame dispatch, or saved-graph migration is introduced. Existing
`BeforeFlatten`/`AfterFlatten` callbacks and their order remain unchanged.

Rejected: a separate water preparation provider, because the existing ordered
registry already represents this operation. Also rejected: running every loader
migration early, because that would change expansion and binding-capture order.
The cost is two additional stage tags and a construction-time registry walk.

Keep `scene_modifier_expand` in the engine: its authoring expansion is separate
from this runtime family preparation. Removing native types from its coupling
routes remains part of the pending runtime seam; this decision does not permit
leaving a physics dependency behind.

Verification: extend `migration_order_matches_table` to include the two runtime
stages; route the existing surface fixture tests through the registered stage.
Those tests check legacy graph preservation, unsupported inputs, and idempotence.
The existing cloned-solid FLIP binding tests cover the second call site. Compile
the engine, then run these focused CPU contracts. Full P5 invariants and landing
remain required after the complete extraction; this is not a separate landing.

#### P5 clock carrier seam

Keep `manifold_physics::clock::SimulationClock`, `stepping::StepInterval` and every
acceptance/CFL/event rule in their present native owners. The graph executor only
needs accepted interval endpoints and numerical-loop position. In
`exec/substeps.rs`, replace `SubstepInterval.interval: StepInterval` with
`start: Seconds, end: Seconds`; retain `ordinal`, `first_iteration`, `iterations`
and `total_iterations` unchanged. Add `duration(self) -> Seconds`, with the exact
existing subtraction `Seconds(self.end.0 - self.start.0)`.
`SubstepClockOutput::single` becomes
`single(duration_port: &'static str, start: Seconds, end: Seconds,
ordinal: u32, total_iterations: u32) -> Self`.

`gpu_flip_domain::substep_clock_output` and `matter_domain::schedule_intervals`
copy the native interval's endpoints into this existing graph carrier. Matter's
native coupling reconstructs `StepInterval::new(start, end)` at its boundary.
The executor and `matter_state` read `timing.duration()`; no duration is inferred
from iteration count. This adds no clock, storage, conversion arithmetic or
allocation. Rejected: relocating native stepping to the engine or foundation,
because the executor is a consumer of accepted time, not its owner.

Re-derive callers with `rg -n 'SubstepInterval|SubstepClockOutput::single|timing\.interval'
crates --glob '*.rs'`. Migrate the production consumers and their existing tests
together, using compilation to find old field accesses. Preserve the unequal
interval, host-sync, pause, reset, export and coupled-progress proofs. The live
clock's section 9 remains authoritative: two fixed accepted intervals at most,
one after a late frame, existing overload discard, ordered hits and exact export.
This refactor does not reopen that policy or claim the pending live HUD work.

Implemented in the T2 lane: engine check and clippy with `gpu-proofs` pass;
45 existing CPU contracts pass across executor substeps, FLIP obstacle timing,
Matter interval scheduling and state publication. GPU behavior and the complete
extraction remain subject to the P5 gates above.

#### P5 CPU wire storage seam

The three family payloads currently repeat a typed `AHashMap<Slot, T>` in both
backends and a `Vec<(Slot, T)>` in the executor. Keep those containers and the
post-evaluate publication point. Add `exec::cpu_values` with
`CpuWireRegistration::new<T: Clone + Send + 'static>()`, collected through
`inventory`. A registration constructs one erased map and one erased write
buffer per backend/executor at construction; individual values remain inline in
their typed containers. Native geometry and field evaluators remain family-owned.

`CpuWireValues` provides `get<T>(Slot) -> Option<T>`, `set<T>(Slot, T)`,
`remove(Slot)` and `clear()`. `CpuWireWrites` provides `push<T>(Slot, T)`,
`clear()` and `commit(&mut CpuWireValues)`. Both implement `Default` using the
registry; duplicate type registrations and access to unregistered types fail
explicitly. Private erased traits expose the typed containers through `Any`;
the registry and container lookups use Rust `TypeId` and `AHashMap`, never a new
serialized identifier. Draining writes retains their vector capacity and order.

`Backend` exposes required `cpu_values()` and `cpu_values_mut()` accessors;
`NodeInputs` exposes `cpu_value<T>(port)` and `cpu_value_slot<T>(Slot)`;
`NodeOutputs` exposes `set_cpu_value<T>(port, value)` and the existing testkit
visibility pattern for `with_cpu_value_writes`. Replace only the three family
payload APIs. Existing scene vocabulary, scalar storage and port tags stay put.
Release/reset removes these values so a recycled slot cannot expose old data.

Register `RigidBody`, `FluidRole` and `FieldValue` in water. The image family's
`vector_fields.rs` contains only native physics field sources/composition; include
it in the water move and remove the image crate's native physics dependency.
This narrows D1's image owner; it does not relocate native field algorithms.

The same closure includes the image family's `euler_step_particles`,
`euler_step_particles_3d`, `apply_radial_burst_to_particles` and
`apply_radial_burst_3d_to_particles`: their evaluation and freeze uniforms use
water's native particle-duration policy and diagnostics. Move these primitives
and their shaders with water, preserving their IDs and bodies. Their mixed
image/water particle-pipeline integration test belongs in the catalog.

Move `smooth_surface_mesh` and `surface_mesh_normals` from scene with their
extent modules and shader. They use water's volume-surface passes, edge ownership
and brick layout. General mesh vocabulary remains in the engine; general mesh
rendering remains in scene. These ownership changes avoid family-to-family
dependencies without adding callbacks or duplicating native policy.

Water-specific test helpers move with water. The direct primitive GPU harness
(`testkit::array_harness`) and generic array/substep fixtures stay in the engine,
so image and scene proofs need no water test dependency. Native field publication
is tested in water; the engine's scheduling proofs use explicit unequal intervals.

Rejected: one boxed payload per write, because it adds frame allocations;
`StateStore`, because its node/owner identity and rebuild lifecycle do not model
resource slots; opaque byte buffers, because they require unsafe layout contracts.
The cost is typed table lookup/downcast plus one erased drain per registered
family type. No locks, per-frame boxes, alternate clock or serialized changes.

Verification: retain existing fluid-role and vector-field round-trip/release
tests and executor publication proof; add focused storage coverage for multiple
types, ordered overwrite, retained capacities and unknown/duplicate registration.
Compile all migrated call sites, then run their existing CPU contracts. Final
water GPU parity and the P5 trace still establish behavior and hot-path cost.

Lane verification: engine/image compile and four-package clippy with
`gpu-proofs` pass. All 46 selected CPU contracts pass, including state carry,
host modulation, field composition, shared geometry and recycled body slots.
The compiler also found the three payload accesses inside `physics_carry`'s
macro; those use the same storage API. No full water GPU parity claim yet.

#### P5 native node interface

Move the 15 native methods from `Primitive` and `EffectNode` into one water-owned
`water::node::PhysicsNode: Send` trait, retaining their signatures and defaults:
`fluid_domain_snapshot`, `rigid_scene_observation`, `set_coupled_physics`,
`set_physics_project_tempo`, `request_physics_samples`,
`set_physics_source_identity`, `capture_coupled_rigid`,
`set_coupled_rigid_inputs`, `coupled_rigid_frame`, `accept_coupled_rigid_frame`,
`physics_impulse_epoch`, `physics_impulse_stamp`, `enqueue_physics_impulse`,
`drain_physics_impulses`, and `drain_discarded_impulses`.
Asset-path/provenance methods remain on the generic traits: scene and image
sources also implement them. Native solvers and payload types do not move.

Reuse `EffectNode`'s blanket `AsAny`, adding its ordinary mutable counterpart
`as_any_mut(&mut self) -> &mut dyn Any`. Water collects
`PhysicsNodeRegistration::new<T: EffectNode + PhysicsNode + 'static>()` through
inventory. Each entry holds its Rust `TypeId` and two checked cast functions.
`get(&dyn EffectNode) -> Option<&dyn PhysicsNode>` and
`get_mut(&mut dyn EffectNode) -> Option<&mut dyn PhysicsNode>` borrow the existing
node; they never create another node or box. An immutable `AHashMap` indexes the
registrations, rejecting duplicates. Water's existing `physics_sample_steps`
construction pass initializes that index before live evaluation.

The four production implementations are `PhysicsWorldNode`, `FluidSurface`,
`GpuFlipDomain` and `MatterDomain`. Move their native method bodies unchanged to
`impl PhysicsNode`; each registers once. Move the two synthetic implementations
with their existing tests. Optional observations and broadcasts skip nodes with
no native interface; validated impulse recipients require it and preserve their
existing error paths. Concrete tests invoke `PhysicsNode` directly; graph-facing
tests continue exercising registered dispatch and receipt ordering.

Rejected: opaque `Any` event messages, because they discard typed admission and
receipt contracts; keeping native defaults on generic traits, because that
retains the dependency being removed. The cost is one registration lookup and
checked cast when accessing a native interface. No mutable shared state, frame
allocations, numerical changes or new clock. Generic coupled scheduling and
runtime state separation must still remove the remaining engine calls into
water before the crate move; this interface is not a completed extraction.

#### P5 paired scheduling seam

The engine owns ordering and resource lifetime; water owns collider masks and
native capture/publication. Replace `CoupledScene` with
`exec::node_pairs::NodePair { first, second, behavior: Box<dyn NodePairBehavior> }`.
`NodePairBehavior: AsAny + Send` requires:
`set_enabled(&self, node: &mut dyn EffectNode, enabled: bool)`,
`before_first(&self, first: &mut dyn EffectNode, second: &mut dyn EffectNode,
second_inputs: Option<&mut EffectNodeContext<'_, '_>>)` and
`after_first(&self, first: &dyn EffectNode, second: &mut dyn EffectNode)`.
Construct the box once per pair; callbacks borrow existing nodes and state.

`Graph::add_node_pair(first, second, behavior)` validates existence and cycles,
rejects duplicate ordered pairs, and enables both participants. `node_pairs()`
returns a read-only slice; `pair_behavior_mut(first, second)` permits prepared
family metadata updates. Water's `add_coupled_scene` merges duplicate collider
masks and re-enables participants as before. Removing a node disables surviving
participants only when no retained pair uses them. A crate-private graph
accessor borrows the behavior and both node instances together for execution.

`NodePairSteps { first_step, second_step, pair_index }` replaces native plan
metadata. Preserve the original graph index when pruning dead pairs. Move the
contraction algorithms and existing tests into `exec::node_pairs`; preserve
liveness closure, stable order, internal-wire rejection, substep exclusion,
resource release and truncation. Generic tests use an ordering-only callback.
Water's `PhysicsPair` retains `RigidImpulseTargets`, missing-input clearing,
capture and accepted-frame publication. Fluid runs first, rigid second. Native
impulse routing considers only pairs whose behavior downcasts to `PhysicsPair`.
Remove the water-named graph/plan APIs rather than leaving forwarding methods.

Add a `GraphInstantiationHook` inventory beside `graph_loader`: named callbacks
take the exact def, registry, `AHashMap<u32, NodeInstanceId>` and mutable graph,
returning `GraphBuildError`. Run in unique name order after wires and before
mesh-rule/budget installation, at today's coupled registration point. Water
registers the existing installer with identical splice-local mapping and errors.
Authored expansion remains in the engine; its native route types are a later
seam. Rejected: native payloads in generic plan metadata or cloned solver state.
Cost: one preparation-time box per pair and borrowed callback dispatch. No new
clock, lock or frame allocation. Verify scheduling, substep, splice, sampling
and coupled playback contracts; final P5 GPU parity and landing still apply.

#### P5 authored impulse routing

Move `RigidImpulseTargets` and `ImpulseTarget` into `scene::impulse`: they are
scene recipient selections, independent of fields, clocks, queues and solvers.
Preserve their serde representations and selection operations. Make `union`
public for the extracted water consumer. `RigidImpulseTargets::BODY_CAPACITY`
is `u64::BITS as usize`; native `MAX_BODIES`, authored slot allocation and
recipient parsing use this one mask bound. Remove the former exports and update
callers directly. Resolved fields, event receipts and simulation stay in water.

Shatter reads an absent parent density from the registered body parameter
definition. Pass its existing registry into preparation and report missing or
non-numeric metadata explicitly. Preserve numeric authored overrides, generated
density values and the native default; do not copy the density constant into
the engine. This lookup happens during graph preparation, once per parent.

Verify exact serialized recipient shapes and slot bounds, existing authored
route/coupling contracts, and Shatter default and override inheritance. This
seam does not establish final water GPU parity or complete the extraction.

#### P5 native mesh and field helpers

Keep shared mesh selection, transforms and triangle partitioning in
`scene::physics_mesh`. Move `prepare_colliders` unchanged to
`water::physics_mesh`; expose the existing `fragments` partition helper for
the water caller. Move the combined partition/collider contract with the
cooker, preserving every assertion. Rendering continues to use the same
selection code without depending on native hull cooking.

Move `scene::vector_field` to `water::vector_field`: its retained-field
interpolation and CPU wire contracts only serve native water consumers.
Update callers directly, with no compatibility exports or new callbacks.
These ownership changes preserve numerical bodies and do not establish the
final crate-move or GPU parity invariants.

The generic executor exposes `execute_cpu_sample_frame` for the existing
caller-selected ancestry and retained parameters. Its scheduling, validation
and resource behavior stay unchanged. A water-owned
`physics_sampling::execute_physics_sample_frame` function holds the existing
`PhysicsAuthoredSampleScope` around that call; water runtime and native proofs
use this function. Generic executor/substep contracts use the CPU method.
This removes the executor's native scope dependency without moving the native
thread-local state, adding a registry, or allocating per sample. Existing
history contracts verify that historical observations retain the native scope
and ordinary observations run after it is restored.

#### P5 runtime state ownership

Consolidate the six native runtime fields in
`water::runtime::WaterRuntimeState`: impulse identity, scene impulse routes,
sample-step mask, retained input snapshot, last observed frame and project tempo.
Its constructor performs the existing mask validation and snapshot preparation
once; both runtime constructors retain their current error reporting. History
sampling is a method on that state with borrowed graph, plan, executor and
bindings; the existing runtime entry observes source assets before calling it.
Water owns the existing after-frame setup observation and reset bookkeeping. Do not change
sampling, delivery, carry eligibility or reset order.

Under `gpu-proofs`, water also owns one `PhysicsSourceState` per effect slot, in the
same immutable construction order. `EffectSlot` retains only generic metadata;
source refresh borrows its node map and card prefix. Construction allocates the
source vector once, with the exact slot count. Frame execution refreshes the
matching source before binding writes and observes instance controls afterward,
as before. No node maps or prefixes are copied. Existing source isolation and
rebuild contracts cover membership and provenance. This step does not authorize
exposing all runtime fields or adding an untyped event dispatch API.

The runtime boundary uses inventory-backed `RuntimeRegistration`, in stable
name order, to validate before compilation and create owned `RuntimeExtension`
state once per graph. Under `gpu-proofs`, preparation captures the unfused
source definition before fusion and installs its result at the existing point
after construction. The engine invokes typed callbacks at the existing frame,
reset, binding, modifier admission and rebuild boundaries. Math View recursion
for tempo, host controls and carry remains engine-owned, in its existing order.

`RuntimeContext` borrows graph, plan and executor plus read-only slot scopes and
the existing identity, dimensions and plan-epoch guards. Slot scopes borrow maps
and bindings; they do not clone them or expose the runtime's caches and UI state.
Water implements these callbacks and exposes `WaterRuntimeExt::water()` and
`water_ref()` for native host operations. Their stack-only views borrow the
registered state; there is no per-frame trait-object construction or new shared
state. Read-only impulse preparation and fluid inspection remain read-only.
Water test observations move with this API. `runtime/` must have no water or
native physics references, and water must not define inherent methods on the
foreign `PresetRuntime` type. Compiler checks and existing history, identity,
carry and impulse contracts validate the seam; the extraction still requires
the full P5 parity and trace gates.

Phasing-completeness check: every D1 crate appears in exactly one phase's deliverables (ui-paint P1a, graph P1, image/scene/compositor P2, nodes P3, water P5); D5 P0; D6 P3; D7 P1/P2; D8 P4; D10 P0; D11 P0; D12 P1a; INV-5's script P0; measurement P4.

---

## 6. Decided — do not reopen

1. Seven crates (D1); `manifold-renderer` deleted at P3.
2. No facade, no re-export, no transitional crate (D4).
3. Scene vocabulary is hub (D2); scene *rendering* and view models are the scene family.
4. `generators/` helpers are hub modules named by what they are (D3); the name `generators` dies.
5. Migrations register via `inventory` with committed tiers (D5).
6. Linking proven by the catalog's references and the census (D6).
7. One test binary per crate; testkit is a hub feature (D7).
8. Visibility widens on compiler demand only; reviewed once in P4 (D8).
9. Water leaves the hub in P5, after the adapter seam; Tier 1 keeps it in `manifold-node-engine::water` (D9).
10. Built-ins are the D11 list, enforced.
11. Layering is a `cargo metadata` test (D12).
12. Astra lanes for P0–P4; Opus lead; P5 is lead work.
13. The engine crate is grouped by job (D13).
14. P1 cuts eight registration seams before the pure move, each with registration parity coverage:
    - Catalog access: `PresetCatalogSource` supplies JSON, cached definitions and enumeration; providers sort by name, first lookup hit wins, visits enumerate all, and duplicate type ids fail tests.
    - Metadata reload: invoke fresh loaders from the existing effect, generator and scene-modifier inventories; preserve merged publication before generation advances. Provider names are required for deterministic ordering.
    - Relight augmentation: scene registers graph augmentation and static parameter targets; runtime resolves bindings during preparation.
    - Array scratch: families register checked private-array sizing, resolved during budget preparation with cutter/fused attribution preserved.
    - Pipeline prewarm: scene registers Math View pipelines; retain the existing Math View installation call; do not move it to general device installation or startup.
    - Mesh assets: scene registers decoding, selection, fitting and translation; engine retains fragment selection and collider preparation.
    - Scene exposure: scene registers its curated node and look metadata for graph construction.
    - Physics source identity: fold the engine integration identity, then registered family identities sorted by name. Each crate hashes only its owned inputs; no new cross-crate source reads. Expected hash changes invalidate old identities.

## 7. Deferred

- **`manifold-graph-def`: lifting `EffectGraphDef`, `ParamManifest`, params and effects types out of `manifold-core`** so the hub stops depending on the DAW model. Trigger: the first external consumer, or Peter's publication call (`PHYSICS_ENGINE_BOUNDARY_DESIGN.md` section 9 (Deferred and Peter's calls)). Serialization-bearing; its own design with the LiveSchool oracle.
- **`manifold-physics-gpu` (PHYSICS_ENGINE_BOUNDARY G1b).** Numerical kernels below `manifold-nodes-water`. Trigger: that design's own.
- **`scene_modifier_expand` out of the hub** behind an expansion registry. Trigger: P5's findings show it shares the migration mechanism; decided there.
- **Per-family `gpu_queue.py` locks.** One GPU, one lock; nothing in this design changes that.
- **Vulkan backend crate.** Out of scope; the split makes `manifold-gpu`'s seam the only one a backend touches (`VULKAN_BACKEND_DESIGN.md`).
