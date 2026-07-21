# Renderer Runtime Decomposition — Wave 3 of the god-file campaign

**Status: PROPOSED — awaiting Peter's design review (execution does NOT start overnight — decisions bus D-28 fences Wave 3 regardless of review state) · 2026-07-22 · Fable**
**Prerequisites:** none hard; Wave 2 (`MODEL_COMMAND_DECOMPOSITION_DESIGN.md`) shares the tooling and should land its pattern first. Campaign register: `docs/ARCHITECTURE_DEBT.md`. Status for this wave lives ONLY on this doc's Status line.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase. Adversarially reviewed 2026-07-22 (fresh Fable agent, full repo verification): 2 HIGH + 4 MEDIUM + 3 LOW findings, ALL upheld and folded below (marked "review H1" etc.); sizes, test shares, ~25 anchors, the determinism argument (zero `file!`/`module_path!`/`type_name` anywhere in the emission path), the serde negatives, the facade viability, and the gltf cohesion verdict verified correct.

**The governing insight: the register's godhood metric (`wc -l`) caught three files whose majority bulk is TEST corpus, not code.** `freeze/codegen.rs` is 51% tests (3,250 of 6,374), `preset_runtime.rs` is 53% (4,326 of 8,175), `gltf_import.rs` is 64% (5,477 of 8,502). The code underneath is, respectively: one compiler stage with a clean two-mode seam, one runtime aggregate with detachable support machinery, and one cohesive importer. The campaign mandate for this wave — cohesion judgment per file BEFORE proposing splits, with the register's non-target list (single compiler stages, cohesive importers) as precedent — produces three different verdicts, recorded in §2: **split modestly / extract support / do not split the code.** Nothing here resembles Wave 1's concern-matrix surgery; proposing one would be manufacturing godhood to justify a campaign wave.

Stage translation: `codegen.rs` decides what WGSL reaches the GPU every frame of a show; `preset_runtime.rs` is the live entry point for effect fusion. The value of this wave is merge-surface and review-surface reduction on exactly those files during the release push — plus recorded verdicts so the campaign stops re-flagging the same three names. Zero pixel/timing change; the fused-WGSL byte-snapshot test is the proof.

**Binding constraints** (DESIGN_AUTHORING §1): *Hot path* — none of the moves touch per-frame bodies; codegen determinism (emitted text = pipeline-cache key, FREEZE_COMPILER_MAP §5) is preserved trivially by pure moves and proven by the byte-snapshot gate. *Persistence* — none: no type in these files serializes into `.manifold` (`ImportReport`/`MergePlan`/`ChainError` are runtime-only). *Thread residency* — untouched (content thread + chain-fusion worker unchanged). *Authority* — `FREEZE_COMPILER_MAP.md` remains the authoritative map; this design reconciles WITH it (its §2 file-map rows get path/size updates in the same landing) and forks none of its content.

Companion docs: `FREEZE_COMPILER_MAP.md` (authority; §2/§5/§10 cited throughout), `MODEL_COMMAND_DECOMPOSITION_DESIGN.md` (Wave 2 — shared tooling + gate pattern), `docs/ARCHITECTURE_DEBT.md` (register), `UI_FUNNEL_DECOMPOSITION_DESIGN.md` (campaign house style).

---

## 1. Audit — what exists (verified 2026-07-22, this session)

| Piece | Where | State |
|---|---|---|
| `freeze/codegen.rs` | 6,374 lines (map §2 says 5,498 — stale, grew), 108 commits since May (highest of the three). Regions: shared emission utils + `CodegenError` :29–222; standalone kernel emission (texture `generate_standalone`/`_ext` :223–891, buffer :964–1256, resolve :1257–1371); fused-region model types (`InputSource`, `FusedVirtualChain`, `RegionNode`, `FusionRegion`, `GeneratedFusion`) :1372–1598; fragment split/rename helpers :1599–1742; fused emission (buffer `generate_fused_buffer` :1743–2274, texture `generate_fused` :2275–3028, `chain_member_args` :3029–3124); `mod dispatch_contract_tests` :3125–3316 (`#[cfg(test)]`); `mod gpu_tests` :3317–6374 (`#[cfg(all(test, feature = "gpu-proofs"))]`) | SPLIT modestly (P3-C, pure moves): standalone vs fused vs shared — the two emission modes are the two active workstreams (conversions churn standalone; fusion features churn fused) |
| codegen churn driver | conversion sweeps + fusion features (P5/P6/P7 waves, relight fusion, marker ABI) — all legitimately this stage; churn is high because BOTH modes are active | |
| `preset_runtime.rs` | 8,175 lines, 65 commits since May (lowest). Regions: chain/slot model (`PresetRuntime` struct :140, `PresetIo`, `EffectSlot` :590–713, relight writes :576–755); segment machinery (`build_segment_cards`, `classify_segment_member`, `segment_run`, `prewarm_chain_segments`, `prewarm_project_chain_segments` :756–934); error types (`JsonGeneratorLoadError` :295–405, `ChainError` :437–575); manifest gate :935–965; `impl PresetRuntime` #1 :966–2686 (try_build, harvest, run, preview/dump instrumentation); `impl PresetRuntime` #2 :2687–3600 (generator constructors `from_json_str`/`from_def`(+`_with_device`), frame context, param application, render, resize, dump-all); `compute_topology_hash` :3601–3661; `OpenGroup`/`close_mix_group` :3662–3699; `SlotAssignment`/`assign_texture2d_slots` :3700–3849; 11 test mods :3850–8175 (review M4; mixed `#[cfg(test)]` and gpu-proofs-gated) | EXTRACT support + tests (P3-R, pure moves); the `PresetRuntime` aggregate itself stays whole (Wave 2 D4's aggregate rule) |
| `gltf_import.rs` | 8,502 lines, 89 commits since May. Code :1–3024: graph-assembly helpers :96–657; `assemble_import_graph` :658; `build_object_group` :702–2008 (one glTF-object → node-group translation); `build_import_graph` :2009–2664; merge-import (`MergePlan`, `merge_import_into_graph`, `assemble_merge_plan`) :2665–3024; `mod tests` :3026–8502 — **64% of the file** | Code is COHESIVE — one importer pipeline, register precedent honored. Tests move out (P3-I, pure move); code does not split |
| gltf churn driver | anim-v2, import-anything interpolation, scene-panel exposure stamping, BUG fixes — all importer work; no second concern hiding | Confirms cohesion verdict |
| Determinism oracle | `fused_wgsl_snapshot_unchanged` (`freeze/markers.rs:439`; FREEZE_COMPILER_MAP §11 edge 1 — built for the marker-ABI refactor, proves a refactor changed zero emitted bytes) + `freeze/reference.rs` golden kernels. Review-verified: zero `file!`/`module_path!`/`line!`/`type_name` hits in any of the three files — nothing in the emission path can leak a path into emitted WGSL or the cache key | EXISTS — the byte-level gate for P3-C |
| `include_str!` in test corpora | codegen `gpu_tests`: 22 hits, relative paths like `include_str!("../primitives/shaders/gain.wgsl")` (first :4049); preset_runtime test mods: ~20 hits (`"../assets/generator-presets/…"`, :5444–:6727, some in plain `#[cfg(test)]` mods); gltf_import: ZERO (its tests use `env!("CARGO_MANIFEST_DIR")` absolute paths) | `include_str!` resolves relative to the containing FILE → moving these test mods one directory deeper REQUIRES a path-depth rewrite on exactly those lines (review H1) — sanctioned as wiring, D6 |
| GPU proof suite | `freeze/proof.rs` ~40 render-two-ways proofs; CLAUDE.md mandates a gpu-proofs run when touching the freeze compiler | Landing gate for P3-C/P3-R |
| Test cfg gates | codegen `gpu_tests` under `#[cfg(all(test, feature = "gpu-proofs"))]`; preset_runtime mixes plain-test and gpu-proofs mods; gltf tests plain `#[cfg(test)]` | Moves carry attributes verbatim — gating preserved by construction |
| Pure-move tooling | `scripts/move_identity_check.py` + census scripts + slice/landing pattern (Waves 1–2) | EXISTS — reuse verbatim |
| Non-target precedent | register: `freeze/proof.rs`, `freeze/region.rs`, `render_scene.rs`, `wgsl_compute.rs` are named cohesive non-targets | The §2 verdicts extend this list, with evidence |

Classification: **exists** — all tooling, all oracles. **Genuinely new** — nothing but directory skeletons. Zero new systems, zero signature changes, zero serde exposure. Negative claims, checked 2026-07-22: no type in the three files carries a serde derive that reaches `.manifold` (`ImportReport`/`MergePlan` are in-memory; `rg 'Serialize' gltf_import.rs preset_runtime.rs` hits are `SerializedParamValue` name matches); no existing `codegen/`, `preset_runtime/`, or `gltf_import/` directories.

---

## 2. Decisions — cohesion verdict per file, then the minimal seams

**D1 — `gltf_import.rs`: cohesive, code does NOT split.** The code half (~2,970 lines) is one pipeline — parse → per-object group assembly → graph assembly → merge-plan — with helpers already factored and churn that is uniformly "importer work". `build_object_group` at ~1,300 lines is long, not god: one object translation, straight-line assembly. The mandate's own test ("a single importer that is merely LONG is NOT a god file") lands here. Action taken anyway: `gltf_import.rs` → `gltf_import/mod.rs` (code, verbatim) + `gltf_import/tests.rs` (`#[cfg(test)] mod tests;` re-declared, bodies verbatim) — the 64% test corpus is what makes the file hostile to edit and review, and extracting it is a zero-risk pure move. Rejected: *splitting `build_object_group` by sub-assembly (materials/animations/skeleton)* — the sub-assemblies share the running node-id/wire state threaded through the function; a split would force a context struct (invented infra) to serve a line count. This file enters the register's non-target list at P3-Z with this verdict as its evidence.

**D2 — `freeze/codegen.rs`: split at the standalone/fused seam.** → `freeze/codegen/`:

| Module | Contents | ~lines |
|---|---|---|
| `mod.rs` | re-exports (`ENTRY`, `CodegenError`, the pub API — external callers in install.rs/region.rs/wgsl_compute.rs/primitive specs keep their paths); shared emission utils: :29–222 (`param_wgsl_type`, `param_word_count`, `param_is_fusable`, `wgsl_safe_field`, `TexDim`/`DimForms`) PLUS `wgsl_storage_token` :272 PLUS the shared buffer helpers :892–963 (`channel_wgsl_ty`, `buffer_element_type`, `emit_buffer_struct` — used by BOTH standalone (:1021–:1279) and fused (:1871–:2521) emission; review H2 caught these stranded in standalone.rs's range) | ~360 |
| `standalone.rs` | `standalone_for_spec`(+`_fmt`), `standalone_for_node`, `generate_standalone`(+`_ext`), buffer + resolve standalone paths :223–1371 EXCEPT the mod.rs carve-outs above | ~1,050 |
| `fused.rs` | region model types :1372–1598, fragment split/rename :1599–1742, `emit_derived_uniform_markers`, `generate_fused_buffer`, `generate_fused`, `chain_member_args` :1599–3124 | ~1,700 |
| `tests.rs` / `gpu_tests.rs` | `dispatch_contract_tests` (attr :3124) and `gpu_tests` (attr :3317) bodies verbatim except D6's sanctioned `include_str!` path-depth rewrites; cfg attributes move onto the mod DECLARATIONS in mod.rs (D6); internal test→subject assignment is an execution deliverable | ~3,250 |

**Named contents win over line ranges** (same reading rule as Wave 2): ranges are dated navigation; the item lists are the contract. Why this is a real seam and not a line-count split: the two modes have disjoint CHURN DRIVERS (every atom conversion touches standalone; every fusion feature touches fused) — that is the load-bearing justification. Consumer sets are NOT fully disjoint (review M3: `region.rs` calls `generate_standalone_ext` :1322/:3761 AND `rename_ident` :1373 from the fused-side helpers; the facade keeps all its paths valid) — the seam is justified by who EDITS the halves, not who calls them. The shared surface is the widened mod.rs util set above. Determinism is untouched by construction (no emission body changes) and PROVEN by `fused_wgsl_snapshot_unchanged` + the golden-kernel drift checks running unmodified. Rejected: *"cohesive, do not split" verdict* — defensible, but the file is the highest-churn target in the campaign register and the seam costs nearly nothing; the honest form of the verdict is "one stage, two modes, one cheap seam — take it, stop there". Rejected: *further splitting fused.rs (buffer vs texture emission)* — they share the region model types and the marker emission helpers; two files would fight over a third within one workstream.

**D3 — `preset_runtime.rs`: the aggregate stays whole; support and tests move out.** → `preset_runtime/`:

| Module | Contents | ~lines |
|---|---|---|
| `mod.rs` | `PresetRuntime` struct + BOTH inherent impls verbatim; `EffectSlot`/`PresetIo`/`RelightParamWrite`/`StringBindingResolution` + the small free fns they lean on (`output_resource`, `build_relight_writes`, `chain_active_effects` :782–817 — explicitly CARVED OUT of segments.rs's range, it is called from both segments code :881 and impl #1 :1012 (review M5), manifest gate) | ~2,940 |
| `errors.rs` | `JsonGeneratorLoadError` + `ChainError` + their `Display`/`Error`/`From` impls + `record_chain_error` :295–575 | ~290 |
| `segments.rs` | `SegmentMember`, `classify_segment_member`, `segment_run`, `build_segment_cards`, `prewarm_chain_segments`, `prewarm_project_chain_segments` :756–934 minus the `chain_active_effects` carve-out | ~150 |
| `build_support.rs` | `compute_topology_hash`, `OpenGroup`/`close_mix_group`, `SlotAssignment`/`assign_texture2d_slots` :3601–3849 | ~250 |
| test files | the **11** test mods (multi_segment :3850, binding_seed :3995, topology_hash :4056, user_binding :4332, bug080_manifest_gate :4675, persistent_slot :4731, generator_input :4843, chain_error :5296, generator_runtime :5391, chain_fusion :6763, segment_prewarm :8132 — review M4) distributed beside their subjects; the mod→file assignment is an execution deliverable under the may-line (each mod names its subject); cfg attributes onto mod declarations (D6); `include_str!` path-depth rewrites sanctioned (D6) | ~4,326 |

`mod.rs` at ~2.9k is this wave's honest ceiling and the same call as Wave 2's `instance.rs`: one aggregate's inherent behavior (build, frame loop, preview/dump instrumentation, generator constructors are facets of `PresetRuntime` operating on its own fields). A per-facet impl split was considered and rejected — Wave 1 D1's "shrinks files without removing the property" failure, and the preview/dump facet is the churn-quietest code in the file. If instrumentation someday becomes its own subsystem, that is a design with an owner type, not a file split. Rejected: *splitting the two impl blocks into two files as-is* — the blocks are an accident of history, not a seam (block 2 mixes generator constructors with frame-context/render methods block 1's callers use).

**D4 — Census, identity, and determinism gates carry over from Wave 2, plus one.** Same INV family: `move_identity_check` zero residue per slice; public-item census (Wave 2 D7's script) over each pre-split file vs its directory; zero external import-site churn (facade re-exports). New, P3-C-specific: **the WGSL byte gate** — `fused_wgsl_snapshot_unchanged` + `reference.rs` golden checks + the full freeze suite (`cargo test -p manifold-renderer --lib node_graph::freeze`) green with zero test-body edits, and a full `gpu-proofs` run at each P3-C/P3-R landing (CLAUDE.md mandate for freeze-touching changes; `cargo test`, never nextest).

**D6 — Test-move mechanics: cfg on the declaration, `include_str!` depth rewrites are sanctioned wiring (review H1/L9).** Moving a `#[cfg(...)] mod x { … }` block to a child file puts the cfg attribute on the `mod x;` DECLARATION in mod.rs with the body unwrapped — the in-repo precedent is `freeze/mod.rs:41–42` (`#[cfg(all(test, feature = "gpu-proofs"))] mod proof;`), also `primitives/mod.rs:155` and `graph_canvas/mod.rs:66`; module paths and nextest counts are unchanged by this form. And because `include_str!` resolves relative to the containing file, the ~42 test-corpus `include_str!` lines in codegen/preset_runtime get a path-depth-ONLY rewrite (`../` → `../../`, prefix change only, filename untouched) — sanctioned as a wiring class. Enforcement: Wave 2's D7a verifier-extension commit (test-mod wiring class) is a PREREQUISITE for P3-C/P3-R test slices, extended here with an `include_str!` prefix-rewrite pattern (allow a paired ±line differing only in leading `../` repetitions; anything else in the line = residue, smuggle-proof) + one fixture each way. gltf_import needs none of this (zero include_str; absolute `env!` paths — review-verified), so P3-I stays a zero-edit move.

**D5 — Map reconciliation is a same-landing deliverable, not a follow-up.** Each landing updates `FREEZE_COMPILER_MAP.md` §2's file-map rows (path, size) for the files it moved — nothing else in the map changes, because nothing about the machine changes. The stale size already present (codegen 5,498 → 6,374) gets corrected in the same edit. Precedent: the supersession-sweep hard rule.

**Consequences, stated honestly:** (1) Three more `.git-blame-ignore-revs` entries; `--color-moved` review per landing. (2) `preset_runtime/mod.rs` (~2.9k) and `fused.rs` (~1.7k) stay large — recorded ceilings with named rationale, pinned by the regrowth test once P3-Z (or Wave 2 P2-Z) builds it. (3) The gpu-proofs landing runs cost real wall-clock on the build-locked machine (~minutes each, serialized by D-14) — the price of touching the freeze path at all; scheduled per landing, not per slice. (4) Wave 3's total code delta is modest by design — if Peter wants deeper renderer surgery (e.g. instrumentation extraction as a subsystem), that is a NEW design conversation, deliberately not smuggled in here.

---

## 3. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| INV-R1 Every commit is a pure move | `scripts/move_identity_check.py`, zero non-wiring residue, per slice + landing — wiring includes D6's two sanctioned classes (test-mod headers via Wave 2 D7a; `include_str!` prefix rewrites), which ship as tooling commits BEFORE the slices that need them |
| INV-R2 Public API surface identical | public-item census old-file vs new-directory, list-diff empty; `git diff --stat` confined to target dirs + mod wiring |
| INV-R3 Emitted WGSL byte-identical | `fused_wgsl_snapshot_unchanged` + `reference.rs` golden checks green, unmodified (P3-C) |
| INV-R4 Fusion behavior unchanged | full freeze suite + `gpu-proofs` run at P3-C/P3-R landings, all proofs green, zero test-body edits beyond D6's named wiring classes |
| INV-R5 Test gating preserved | cfg attributes preserved in D6's declaration form (move_identity + D7a class cover); `cargo nextest run --workspace` count unchanged (no gpu test leaks into the default sweep) |
| INV-R6 No regrowth | `godfile_regrowth` ceilings for all new modules at P3-Z — ⚠ VERIFY-AT-IMPL: the test is BUILT by Wave 2 P2-Z (it did not exist as of 2026-07-22; Wave 2 review M4); if Wave 2 hasn't landed it yet, P3-Z builds it |
| INV-R7 Map stays true | landing diff includes the FREEZE_COMPILER_MAP §2 row updates (D5); reviewer checks presence |

---

## 4. Phasing

Order: **P3-I → P3-C → P3-R → P3-Z** (cheapest/safest first; the tests-only move re-proves the machinery on this crate before the freeze-path landings). One phase = one session. Same execution pattern as Wave 2 when Peter green-lights it (NOT overnight, D-28). Anchors re-derived at entry; counts differ → stop and re-list.

**P3-I — `gltf_import.rs` tests out** *(pure move)*. Entry: `wc -l` re-verify; `rg -n 'mod tests' gltf_import.rs` (expect :3026 area); `rg -c 'include_str!' gltf_import.rs` → 0 (D6's zero-edit premise). Read-back: restate D1/D6 + forbidden list. Gate: `cargo nextest run -p manifold-renderer` green, zero body edits; public-item multiset census; file gone (`test -f`/`test -d` form). Demo: none — L1.
**P3-C — `codegen/` split** *(pure moves)*. Entry: re-derive D2 anchors; confirm Wave 2 D7a verifier class landed (else it is this phase's S0, with the D6 `include_str!` extension); capture censuses. Read-back: restate D2/D4/D6. Slices per D2's table, tooling commit first. Gate: freeze suite + INV-R3 byte gate (lane); full gpu-proofs at landing; multiset census; clippy `-p manifold-renderer`. Demo: none — L1.
**P3-R — `preset_runtime/` extraction** *(pure moves)*. Entry: re-derive D3 anchors (11 test mods); D6 tooling present. Read-back: restate D3/D6. Gate: as P3-C minus the byte gate (this file emits no WGSL); gpu-proofs at landing (chain build is fusion's live entry point). Demo: none — L1.
**P3-Z — Ceilings + supersession**: **create-or-extend `godfile_regrowth`** (by that file/test name — review L7; Wave 2 P2-Z builds it first if it lands first) with ceilings for all new modules; register update (Wave 3 rows → this doc; `gltf_import` onto the non-target list with D1's verdict; vocabulary row N/A — this wave introduces no new layer vocabulary, deliberately); map-row completeness check (INV-R7); status flip; sediment-notes file (same D8 policy as Wave 2, by reference). Demo: none — L1.

Phasing-completeness: D1→P3-I; D2/INV-R3→P3-C; D3→P3-R; D4→every gate; D5→each landing, checked at P3-Z. Nothing deferred.

**Forbidden moves, wave-wide:** any emission-body edit sharing a commit with a move · touching `region.rs`/`install.rs`/`proof.rs` beyond `use`-path wiring · reformatting WGSL string literals ("cleanup" that changes emitted bytes = the cache key) · splitting `build_object_group` · per-facet splits of `PresetRuntime` · new traits/context structs · landing a freeze-touching phase without the gpu-proofs run.

## 5. Decided — do not reopen

1. `gltf_import` code is cohesive: tests out, code intact, non-target list entry. (D1)
2. `codegen` splits at exactly the standalone/fused seam; no buffer-vs-texture sub-split. (D2)
3. `PresetRuntime` aggregate stays whole; errors/segments/build-support/tests move out. (D3)
4. The WGSL byte-snapshot + gpu-proofs runs are non-negotiable landing gates for freeze-touching phases. (D4)
5. FREEZE_COMPILER_MAP is reconciled in-landing, never forked. (D5)
6. Test moves use cfg-on-declaration; `include_str!` prefix rewrites and test-mod headers are the ONLY sanctioned test-line wiring, verifier-enforced. (D6, review H1/L9)
7. The codegen seam is justified by churn-driver disjointness, not consumer disjointness; the shared buffer helpers :892–963 live in mod.rs. (review H2/M3)
8. Wave 3 execution waits for Peter (D-28); this doc lands PROPOSED.

## 6. Deferred

None. (Instrumentation-as-subsystem and any deeper renderer surgery are explicitly NOT designed here — they would be new designs with their own docs, per D3's rationale, not revival triggers on this one.)
