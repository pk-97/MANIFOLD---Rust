# glTF Animation Runtime v2 — file-backed keyframes, hierarchy-true posing

**Status:** APPROVED design, not built · 2026-07-18 · Fable 5 (Peter approved the direction and Sonnet execution in-session: "root level and fundamental fix to get optimal perf")
**Prerequisites:** GLTF_ANIMATION_DESIGN.md A1–A4 (SHIPPED — this doc supersedes its keyframe-STORAGE decision, keeps its runtime surface)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

**Governing insight:** A1–A4 shipped the right performer surface (progress/rate/clip/loop/retrigger as graph params) on the wrong storage: keyframes baked into `ParamValue::Table` params inside the graph def, linearly scanned per joint per frame. On real store assets that storage is the whole disease — the dragon fixture (52 clips, 5.41 M keyframes) costs **5.2 GB peak RSS** (measured 2026-07-18, `/usr/bin/time -l` on `render-import`), duplicated per material object, retained in the registered preset after the layer is deleted, and sampled with per-frame linear scans that reach billions of row reads (BUG-190's 370 ms BrainStem frame and BUG-189 are the same family). The fix removes the class: **the graph def carries only a file path + selection; keyframe payload lives in a shared, file-backed, flat cache; every lookup is a binary search.** A second, smaller class-fix rides along: rigid animation on multi-node-per-material objects (today silently "left static", `gltf_load.rs:2492`) is expressed through the already-shipped skinning path instead of the one-node-per-object TRS hack.

Stage translation: drop any store-bought animated asset in, it plays at frame rate; delete the layer and the memory actually comes back; multi-part scenes animate all their parts.

## 1. Audit — what exists (verified 2026-07-18, session-live reads)

| Piece | Where | State |
|---|---|---|
| Keyframe tables baked into def | `gltf_import.rs:965` (`translation_tracks` insert), `build_skeleton_pose_tables` :292, `build_morph_weight_table` :417 | THE problem — payload in def, per material object |
| Linear scans per frame | `gltf_anim_shared.rs:176` (`row_range_for_compound_key`), `:146` (`row_range_for_key`), `clip_duration` :132, `mat4_from_table` in `gltf_skeleton_pose.rs:209` | All O(rows) from row 0, per joint per track per frame |
| CPU samplers | `gltf_animation_source.rs`, `gltf_skeleton_pose.rs`, `gltf_morph_weights.rs` | Keep node identity + ports/params surface; replace data source |
| Loop/trigger/progress logic | `gltf_anim_shared.rs` (`LoopMode`, `TriggerLatch`, `resolve_progress`) | Correct, keep verbatim |
| File-backed background load precedent | `gltf_morph_deltas_source.rs:96-162` (`pending_load` mpsc pattern; A3 added it precisely because delta payload "doesn't fit the import-time Table convention") | The house pattern this design generalizes |
| Parsed animation model | `gltf_load.rs:1467` `GltfNodeAnimation`, `:1506` `GltfObjectAnimation`, `parse` of all clips already exists (A4) | Parser is fine; only its DESTINATION changes |
| Skinning GPU path | `skin_mesh.rs` (codegen-path, palette via BufferGather), `gltf_skinned_mesh_source.rs` | Shipped, untouched — P3 reuses it for rigid multi-node |
| Multi-node rigid scope hole | `gltf_load.rs:2492` (multi-node material → static), `:1700` (multi animated ancestors → static) | Silent degradation this design deletes |
| Clip cap | `clip_index` param range `(0, 31)` in all three samplers | Dragon has 52 clips; cap is arbitrary |
| Mesh/texture source gating | `gltf_mesh_source.rs`, `gltf_texture_source.rs`, `gltf_skinned_mesh_source.rs` | Verified correctly change-gated this session — NOT part of the problem; do not touch |

Existing/one-wire-away/new: the parser, samplers' math, loop/trigger machinery, skinning path all EXIST. Genuinely new: the shared animation cache (one struct + one loader fn), the flat track layout, and the node-slot palette for rigid multi-node objects.

## 2. Decisions

- **D1 — Keyframe payload never lives in the graph def.** The def stores: `path` (stringBinding, same convention as every gltf source), clip metadata (`clip_durations` stays — it is tiny and UI-relevant), topology scalars (`joint_count`), and selection params. All track data (`translation/rotation/scale_tracks`, per-target weight tracks, `joint_parent/root_world/inverse_bind` tables) moves to a runtime cache loaded from the file. Rationale: payload-in-def is the root cause of the 5.2 GB residency, project.json bloat, snapshot/undo weight, and delete-doesn't-free. Rejected: keeping tables but sharing them via `Arc` interning — still bloats serialization, still per-def, and dedup-by-accident; the class fix is payload-out-of-def.
- **D2 — One shared `GltfAnimCache`, `Weak`-held, background-loaded.** New module `crates/manifold-renderer/src/node_graph/gltf_anim_cache.rs`:
  ```rust
  pub struct GltfAnimSet {          // one per file, immutable after load
      pub clips: Vec<AnimClip>,     // index == glTF animations[] index
      pub skins: Vec<SkinTopology>, // joint node indices, parents, inverse binds, bind TRS
      pub node_parents: Vec<i32>,   // whole-scene node hierarchy
      pub node_bind_trs: Vec<BindTrs>,
  }
  pub struct AnimClip {
      pub duration_s: f32,
      pub channels: Vec<Channel>,   // sorted by target node index
  }
  pub struct Channel {
      pub target_node: u32,
      pub kind: ChannelKind,        // Translation | Rotation | Scale | Weights{target_count}
      pub times: Vec<f32>,          // sorted; binary-searched
      pub values: Vec<f32>,         // flat SoA, stride by kind
  }
  ```
  Cache: `static ANIM_CACHE: Mutex<HashMap<PathBuf, Weak<GltfAnimSet>>>` inside the module, entries loaded on a spawned thread (the `gltf_morph_deltas_source.rs:96` mpsc pattern), primitives hold the `Arc` in `extra_fields`. `Weak` means the last node dropping (layer deleted, preset unloaded) frees the payload — the delete-recovers-memory property is structural, not a cleanup pass. **This is the design's one piece of new shared state, named per the CLAUDE.md rule and approved with this doc:** a coarse `Mutex` around a tiny map, touched only at load/drop (never per-frame), content thread + loader threads only. Rejected: per-primitive private copies (the current shape, ×N duplication); a cache on `PresetRuntime` (two layers using one file across two presets would still duplicate, and plumbing a context handle through `EffectNodeContext` is a wider diff for less sharing).
- **D3 — Every time lookup is a binary search on a contiguous slice.** `Channel::times` is one flat sorted `Vec<f32>`; sampling is `partition_point` + lerp/slerp (the exact math `gltf_anim_shared.rs` already proves — reuse `sample_*` interpolation bodies, retarget them from `TableData` rows to slices). Per-frame cost for the dragon: ~640 channels × log₂(keys) ≈ tens of thousands of float compares, sub-0.1 ms. Rejected: per-channel playback cursors — O(1) amortized but stateful, order-sensitive under scrubbing/retrigger, and the binary search is already far below budget.
- **D4 — Rigid multi-node animation goes through the skinning path (node-slot palette).** For a material object whose geometry comes from >1 node with any animated node, the importer emits: per-vertex `joints = [node_slot, 0,0,0]`, `weights = [1,0,0,0]` (the flatten already knows each vertex's source node), vertices in LOCAL node space, and a pose source that outputs per-slot `world(node) × inverse_bind(node)` matrices — mathematically identical to skinning with rigid single-joint binding, running on the shipped `node.skin_mesh` GPU kernel. The pose evaluation composes the REAL scene hierarchy from `GltfAnimSet::node_parents` (top-down, one pass, memoized — the `resolve_world` shape at `gltf_skeleton_pose.rs:232`), which also deletes the "two animated ancestors" hole (`gltf_load.rs:1700`) — composition is just the parent chain working. Rejected: splitting import objects per animated node — changes the object model, the scene panel, and material batching for a case the skin path expresses with zero new GPU code. Rejected: per-vertex CPU re-transform — CPU skinning, forbidden by GLTF_ANIMATION D2.
- **D5 — The three sampler nodes keep their identity, ports, and card surface.** `node.gltf_animation_source`, `node.gltf_skeleton_pose`, `node.gltf_morph_weights` keep type ids, inputs (`progress`/`clip_index`/`trigger_count`), loop/rate/retrigger behavior, and card knobs — A4's performance surface is settled. They gain a `path` stringBinding and (pose) a `skin_index` / (D4) node-slot mode; they lose the track/topology Table params. Presets and projects saved after this design are small; presets saved BEFORE it carry baked tables → load-time migration (P2) strips the dead Table params and stamps `path` from the group's sibling mesh-source stringBinding; if no sibling path resolves, the node keeps its tables inert and logs one warning naming the object (inert-but-present per the round-trip corollary — never silent drop, never a parallel sampling path reading old tables).
- **D6 — Clip cap follows the file.** `clip_index` param range becomes `(0, 255)`; the importer stamps the card knob's range to the object's actual clip count. Rejected: unbounded — a range is UI contract, 255 is past any real asset.

## 3. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| No keyframe payload in any def the importer emits | Negative gate: `rg -n '"translation_tracks"\|"rotation_tracks"\|"scale_tracks"\|weight_tracks' crates/manifold-renderer/src/node_graph/gltf_import.rs` → zero hits after P2; plus test `imported_def_json_stays_small` — serialize the dragon-scale synthetic import def, assert < 256 KB |
| Sampling never linear-scans keyframes | `row_range_for_key`/`row_range_for_compound_key` DELETED (`rg` zero hits, P2); slice samplers take `partition_point` — reviewed shape, plus perf test below |
| Dragon-scale posing stays under budget | Test `pose_sampling_dragon_scale_under_1ms` (P1): synthetic AnimSet (52 clips × 630 channels × ~160 keys), 300 joints, one full pose sample < 1 ms release / < 8 ms debug |
| Deleting the last referencing node frees the payload | Test `anim_cache_drops_when_last_arc_drops` (P1): load, drop all Arcs, assert `Weak::upgrade()` is `None` |
| Multi-node animated objects animate (no silent static) | P3 gate: the `:2492` report-line branch deleted; held-out multi-node fixture renders 4 pairwise-distinct poses |
| Old projects load without panic or silent behavior change | P2 round-trip + migration tests (save-old-shape → load → animate) |

## 4. Phasing (one session each, Sonnet executor; orchestrator re-derives anchors at phase start)

- **P1 — Cache + skeleton pose vertical slice.** Build `gltf_anim_cache.rs` (D2 types, loader reusing `gltf_load.rs`'s existing parsed structures, mpsc background pattern per `gltf_morph_deltas_source.rs:96`), slice-based samplers in `gltf_anim_shared.rs` (binary search, D3). Rewire `node.gltf_skeleton_pose` onto it: `path` param + `skin_index`, tables ignored, `Arc<GltfAnimSet>` + pending-load in `extra_fields`. Importer stamps `path`/`skin_index` on pose nodes (keeps emitting tables until P2 — additive, nothing breaks). Gate: existing skinned goldens (CesiumMan/Fox 4-pose) still pass byte-identical or visually-equal (re-golden only with a stated reason); `pose_sampling_dragon_scale_under_1ms`; `anim_cache_drops_when_last_arc_drops`; clippy `-p manifold-renderer`. Forbidden: touching `skin_mesh`, mesh/texture sources, or sampler port surfaces.
- **P2 — All samplers file-backed; importer stops emitting payload; migration; clip cap.** `gltf_animation_source` + `gltf_morph_weights` onto the cache; delete table-build code (`build_skeleton_pose_tables`, `build_morph_weight_table`, track-table inserts) and the table-range scan fns; D5 load-migration; D6 cap. Gate: full A1–A4 golden suite green (BoxAnimated, CesiumMan, Fox, AnimatedMorphCube, MorphStressTest); round-trip incl. an OLD-shape fixture def; the two negative `rg` gates from §3; def-size test; Fox 3-clip + a >32-clip synthetic clip-selection test.
- **P3 — Rigid multi-node via node-slot palette (D4).** Flatten emits node-slot joints/weights for multi-node animated objects; pose source in node-slot mode (whole-hierarchy compose); importer wires through `skin_mesh`; delete the `:2492` left-static branch and the `:1700` ancestor bail (composition handles it). Gate: `BoxAnimated.glb` (its static+animated two-node scene) and one held-out multi-node fixture the builder doesn't develop against render 4 pairwise-distinct animated poses; report-lines for the deleted branches gone (`rg` zero hits); existing single-node and skinned goldens unchanged.
- **P4 — Acceptance on the real assets + sweep.** In main checkout: `render-import` the dragon at 4 times (distinct poses, PNGs read by orchestrator — L2); peak RSS measured < 1 GB (vs 5.2 GB baseline); re-measure BUG-190 BrainStem frame time and record; import-to-first-frame wall time recorded. Supersession sweep per CLAUDE.md: GLTF_ANIMATION_DESIGN status note (storage superseded), BUG_BACKLOG entries (new BUG for this work + BUG-189/190 updates), NODE_CATALOG regen if params changed, memory pointers. Demo: click-script for Peter (import dragon, play, switch clips, delete layer, watch memory).

## 5. Decided — do not reopen
1. Payload out of def; file-backed shared cache, `Weak`-held (D1/D2).
2. Binary search on flat slices; no cursors, no per-frame linear scans (D3).
3. Multi-node rigid rides the skinning path; no object-model split, no CPU skinning (D4).
4. Sampler node identities and A4's performer surface unchanged (D5).

## 6. Deferred
- Clip blending/crossfade (unchanged from GLTF_ANIMATION D4; trigger: Peter names a need).
- Mesh/texture payload dedup across objects (mesh sources are correctly gated; trigger: a measured mesh-side memory problem).
- Cursor-based sampling (trigger: a measured budget miss D3's binary search can't close).
- `KHR_animation_pointer` targets (unchanged D5 deferral).
