# glTF Animation — imported clips as performable motion (node TRS, skinning, morphs)

**Status:** APPROVED · 2026-07-16 · Fable 5 (Peter approved) · A1 SHIPPED 2026-07-16 (rigid TRS animation vertical slice: `node.gltf_animation_source`, beat-drive default, saw-LFO loop gesture, four-phase goldens, save/reload round-trip — BUG-187 logged, blocks the `AnimatedColorsCube`-style `KHR_animation_pointer` held-out case) · A2 SHIPPED 2026-07-16 (skinning vertical slice: `node.gltf_skinned_mesh_source` + `node.gltf_skeleton_pose` + `node.skin_mesh`, codegen-path + parity test, CesiumMan/Fox deform correctly, hot-path 5-7ms/frame — BUG-190 logged, `BrainStem.glb`'s 24-skinned-object case measures ~370ms/frame, NOT a named gate fixture, does not block)
**Prerequisites:** GLB_XFAIL_BURNDOWN_DESIGN.md P2 (owns the BUG-170 crate-bump verdict; its D8 may hand this doc three pointer-animation assets). No dependency on GLTF_MATERIAL_EXTENSIONS_DESIGN.md — the two can execute in either order.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's ask (2026-07-16): *"the ability to import animated glb files and scenes too."* Today animated glbs import as frozen statues: the importer reads no `animations`, `skins`, or morph-target data (verified 2026-07-16 — zero hits for JOINTS/WEIGHTS/animation parsing in `gltf_load.rs`/`gltf_import.rs`; `GLB_CONFORMANCE_DESIGN.md` §7 deferred item 7 scoped it out).

Honesty about certification: this doc barely moves the conformance number — most animated Khronos assets (`BoxAnimated`, `CesiumMan`, `Fox`, `BrainStem`, `RiggedFigure`, `MorphPrimitivesTest`…) already **pass statically**, because the suite's checks render one converged frame. The value is the instrument, not the scoreboard.

**The instrument frame is the design's spine:** an imported animation must land as a *performable* thing, not a video-player timeline. The clip's progress is a param — scrubbable, LFO-able, audio-modulatable, MIDI-triggerable, beat-quantized. A walking character retriggered on every kick, a blooming flower scrubbed by a fader, a loop time-locked to bars. glTF stores seconds; MANIFOLD's primary time model is beats — the seconds→beats mapping is a decision (D3), not an accident.

## 1. Audit — what exists (verified 2026-07-16; RE-DERIVE at execution)

```
rg -n 'JOINTS_0|WEIGHTS_0|read_morph|animations\(\)|skins\(\)' crates/manifold-renderer/src/node_graph/gltf_load.rs   # expect: still zero hits
rg -n 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/{morph_mesh,bend_mesh,displace_mesh,gltf_mesh_source}.rs
rg -n 'anim_progress|trigger_count' crates/manifold-renderer/src/node_graph/effect_runtime.rs | head
```

Snapshot: meshes flow through the graph as first-class data — `gltf_mesh_source` → deform atoms (`morph_mesh`, `bend_mesh`, `displace_mesh` — MESH_DEFORM_AND_CURVE_GEOMETRY, SHIPPED 2026-07-11) → `render_3d_mesh` / `render_scene`. `PresetContext` already carries `time`, `beat`, `anim_progress`, `trigger_count` per frame. Per-object transforms reach `render_scene` as graph params (the import graph wires them), so animating a rigid node = animating params — machinery that exists. Genuinely new: keyframe storage + sampling, skinning (JOINTS_0/WEIGHTS_0 → joint palette → per-vertex blend), and morph-target weight animation.

## 2. Decisions (direction-level; each gets full treatment in the executing phase's re-derivation)

- **D1 — Animation is graph-native: a clip source node + existing param wires.** Import produces `node.gltf_animation_source`: inputs a progress scalar (0..1), outputs the sampled values — per-node TRS as transform params, morph weights as scalars — wired to the same object params the static importer already sets. Sampling is CPU-side, content-thread, per-frame, allocation-free after load (pre-sorted keyframe tracks, binary-search + lerp/slerp/cubic per spec). Rejected: a parallel "animation player" subsystem outside the graph, because the graph IS the modulation system — anything else would re-invent param binding and lose the performance surface for free.
- **D2 — Skinning is a fusable GPU mesh atom: `node.skin_mesh`.** Per-vertex joint blend (4 joints/weights per spec) is a barrier-free pure per-element kernel — it MUST ship on the freeze codegen path per the standing all-nodes-fusable rule (CLAUDE.md; `docs/ADDING_PRIMITIVES.md` §"The codegen path is mandatory"). Joint matrices arrive as a buffer input computed CPU-side from the sampled skeleton pose (matrix palette is small — spec-typical ≤ 256 joints; `BrainStem` is the stress case, re-derive its joint count). `MeshData` grows optional JOINTS_0/WEIGHTS_0 attributes (⚠ VERIFY-AT-IMPL: MeshData's attribute model — extend the way UVs/normals ride today, one owner, no parallel skinned-mesh type). Morph targets reuse the `morph_mesh` shape (positions-delta blend) with imported targets. Rejected: CPU skinning, because a 50k-vertex character per frame on the content thread is hot-path allocation/latency the two-thread model exists to avoid.
- **D3 — Seconds→beats at import, performer-facing progress at runtime.** The clip's duration in seconds is metadata; the runtime surface is normalized progress (0..1) plus a default drive: `progress = wrap(beat × rate / clip_beats)` where `clip_beats = duration_s × bpm / 60` computed live from the transport (beats primary; Seconds only at the glTF edge — the house time invariant). The performer sees: progress (scrub/modulate), rate, loop mode (loop / once / ping-pong), and a retrigger input (trigger resets phase — the phantom-clip/trigger precedent). Rejected: storing a beats conversion at import time, because it bakes a BPM into the asset and goes wrong the moment the tempo changes live — the exact class of bug the beats-primary rule exists to kill.
- **D4 — Multi-clip glbs expose a clip selector param** (glTF `animations[]` is a list; `Fox` ships three). Inline-mux option-table pattern (`feedback_inline_mux_option_table_params`). Blending between clips is Deferred — trigger: a real performance need Peter names, not spec completeness.
- **D5 — BUG-170's pointer-extension assets** (`KHR_animation_pointer`, `KHR_node_visibility`) join this doc's scope only if the burn-down's crate bump fails; animation *pointer* targets (animating arbitrary properties, not just TRS/weights) are Deferred regardless — trigger: an asset Peter actually wants that uses them.

## 3. Phasing (conformance level — one session each; full briefs written by the executing orchestrator against re-derived inventory, per STANDARD §9)

- **A1 — Rigid animation vertical slice.** Parse `animations[]` (TRS channels only), `node.gltf_animation_source`, default beat-drive, progress param. Gate: `BoxAnimated.glb` visibly animates in a headless multi-frame render (PNG sequence at progress 0/0.25/0.5/0.75 — four distinct goldens); save→reload→animate round-trip (STANDARD §5 round-trip gate); performer-gesture line: progress driven by a saw LFO loops the clip cleanly at the wrap point. The vertical slice runs model → graph → pixels before anything else lands — DESIGN_AUTHORING §7.
- **A2 — Skinning.** JOINTS/WEIGHTS through `MeshData`, `node.skin_mesh` (codegen path + generated-vs-hand parity test, mandatory), skeleton pose sampling. Gate: `CesiumMan`/`Fox` animate; parity test green; hot-path check (`MANIFOLD_RENDER_TRACE=1`, no frame >20ms — STANDARD §5 content-thread gate).
- **A3 — Morph-target animation.** Imported targets + weight channels through the `morph_mesh` shape. Gate: `AnimatedMorphCube` + `MorphStressTest` animate; four-phase goldens.
- **A4 — Performance surface.** Clip selector (D4), loop modes, retrigger wiring, perform-UI exposure per `feedback_param_values_is_performance_surface`. Gate: L3 ui-flow driving retrigger + scrub; performer gesture: retrigger on a MIDI note fires the clip from zero within one frame.

## 4. Decided — do not reopen
1. Animation lives in the graph as a source node + param wires; no standalone player subsystem (D1).
2. Skinning is GPU, fusable, codegen-path; no CPU skinning, no plain-WGSL boundary kernel (D2).
3. Runtime time surface is beats/progress; no baked seconds→beats conversion (D3).

## A1 Phase Brief (written 2026-07-16, orchestrating session, re-derived inventory)

**Entry state:** `origin/main` HEAD `39bff66c` (E6 SHIPPED). `tests/fixtures/gltf/khronos/BoxAnimated.glb` present. `AnimatedCube`/`AnimatedTriangle` (named in the handoff prompt) have **no glTF-Binary variant at the Khronos pin** (`docs/GLB_CONFORMANCE_STATUS.md:155-156`) — not fetchable, not a blocker; `BoxAnimated.glb` alone is the doc's own A1 gate fixture (§3).

**Read-back / re-derived inventory:**
- `crates/manifold-renderer/src/node_graph/gltf_load.rs` — zero hits for `animations()`/`skins()`/JOINTS/WEIGHTS (confirmed live, matches §1's audit). Add animation-track parsing here, alongside the existing mesh-flatten parse — same "one parse entry" doctrine as `MANIFOLD_SUPPORTED_EXTENSIONS`/`the_one_parse_entry` (file header).
- `crates/manifold-renderer/src/node_graph/gltf_import.rs::build_import_graph` — each material becomes a node **group** containing its own `node.transform_3d` (line ~1008-1020), currently seeded with a static recenter translation only (`pos_x/y/z = -center`). Critically: **`node.transform_3d`'s all nine TRS params (`pos_x/y/z`, `rot_x/y/z`, `scale_x/y/z`) are ALREADY port-shadowed by same-named optional scalar input ports** (`crates/manifold-renderer/src/node_graph/primitives/transform_3d.rs`). D1 — "animating a rigid node = animating params" — is therefore: wire a new source node's per-channel scalar outputs into this existing `transform_3d`'s input ports. No change to `render_scene` needed.
- `node.beat_ramp` and `node.lfo` (shape=Saw, in `LFO_SHAPES`) already exist — the saw-LFO performer gesture (§3 A1 gate) wires an existing `node.lfo` (shape=Saw) into the new source node's `progress` input port; no new LFO primitive needed.
- `ParamValue::Table(Arc<TableData>)` (`parameters.rs:111` `TableData{ rows: Vec<Vec<f32>>, cols }`) is the existing vocabulary for small 2D numeric blobs and is Table params' proven serialization path (V1/V2 project formats already round-trip `ParamValue` variants) — use it to carry parsed keyframe tracks (one row per keyframe: `[time_s, x, y, z]` for translation/scale, `[time_s, x, y, z, w]` for rotation quaternion) rather than inventing a new param/wire type.
- **Known scope boundary, not solved this phase:** glTF animation channels target scene-graph *node* indices; MANIFOLD's import graph groups objects by *material* (`summarize_node` keys by `material().index()`, world-combining node instances). For `BoxAnimated.glb` (one node, one mesh, one material) this is 1:1 and the vertical slice is unaffected. Multi-node-per-material assets (instancing) are out of scope for A1 — re-derive at A2/A4 if a real asset needs it.

**Deliverables:**
1. `gltf_load.rs`: parse `document.animations()` into a per-animation, per-node list of TRS keyframe tracks (translation/rotation/scale, each optional per node), attached to `GltfImportSummary` (new field, e.g. `animations: Vec<GltfAnimationInfo>`). Held-out input: the gate fixture list already includes `AnimatedColorsCube.glb`/`AnimatedMorphCube.glb` in `tests/fixtures/gltf/khronos/` — parse-only (not wired) smoke test against `AnimatedColorsCube.glb` as the held-out asset (translation/rotation channels only; ignore its color-animation extension) proves the parser isn't shaped around `BoxAnimated` alone.
2. New primitive `node.gltf_animation_source` (`crates/manifold-renderer/src/node_graph/primitives/gltf_animation_source.rs`, CPU-only, `boundary_reason: NonGpu`, same family as `beat_ramp`/`lfo`): inputs `progress: ScalarF32` (0..1, port-shadowed, default beat-drive per D3 when unwired: `wrap(beat * rate / clip_beats)` reading `FrameTime::beats`, `clip_beats` computed from a `duration_s` param + live BPM); params carry the keyframe `Table`s (one per animated channel) plus `duration_s`; outputs the nine scalars `pos_x/y/z, rot_x/y/z, scale_x/y/z` (binary-search + lerp for translation/scale, slerp for rotation quaternion→Euler at sample time, per spec) so they wire directly into an object's `transform_3d` input ports. Channels absent from the clip pass through as the node's static default (0 for pos/rot, 1 for scale) — never fabricate motion for an unanimated channel.
3. `gltf_import.rs::build_import_graph`: when `summary.animations` is non-empty for an object's source node, insert one `node.gltf_animation_source` per group, wired into that group's `transform_3d` scalar input ports (additive to the existing static recenter — recenter stays as the node's own param default, the animation source's output overrides at runtime same as any port-shadow).
4. Round-trip: `Table` params and the new node type must survive V1 JSON save→reload (existing param-serialization path — no new format work expected, but the gate must prove it, not assume it).
5. Four-phase PNG goldens: `BoxAnimated.glb` imported and rendered headless at progress 0 / 0.25 / 0.5 / 0.75 — four visibly distinct PNGs (the box's translation animation moving it across frame).
6. Saw-LFO performer gesture: `node.lfo` (shape=Saw) wired into `gltf_animation_source.progress`, rendered across a full LFO cycle — the clip must loop cleanly at the wrap point (frame at progress≈0.99 and progress≈0.01 must be visually continuous, not a snap-back discontinuity — verify the sampler wraps `t` into `[0, duration_s)` before lerping, not clamps).

**Gate:**
- Positive: a new `#[test]` in `gltf_import.rs` (or a sibling test module) rendering the four-phase PNG sequence for `BoxAnimated.glb` to `tests/fixtures/gltf/goldens/` (follow the existing `imported_*_renders_faithfully_to_png` naming/goldens convention) — four distinct, non-identical PNGs. A round-trip test: build the import graph, serialize via the V1/V2 project path, reload, re-render at progress 0.5, confirm pixel match with the pre-reload progress-0.5 render. A saw-LFO loop test asserting near-progress-0 and near-progress-1 renders are near-identical (loop continuity).
- Negative: `rg -n 'JOINTS_0|WEIGHTS_0|skins\(\)'` in this phase's diff returns zero hits (A1 is TRS-only, skinning is A2 — don't scope-creep).
- Test scope: focused — `cargo test -p manifold-renderer --lib gltf_import`, `... gltf_load`, `... primitives::gltf_animation_source` (new render/round-trip tests are GPU-touching via `render_scene`'s existing headless path — check whether the existing `imported_*_renders_to_png` tests already require `--features gpu-proofs`; match that convention, don't invent a new one).
- Clippy: `cargo clippy -p manifold-renderer -- -D warnings` (worktree-scoped).

**Forbidden moves:** a parallel "animation player" outside the graph (D1, decided); baking seconds→beats at import (D3, decided); CPU skinning or scope-creep into A2 (D2/A2 boundary); silently dropping a channel type A1 doesn't handle (fail loudly or leave inert-but-present, per the round-trip corollary); synthesizing the keyframe-sampling math from memory instead of implementing straight off the glTF spec's defined interpolation (LINEAR only for A1 — CUBICSPLINE/STEP are Deferred unless `BoxAnimated` needs them, re-derive at execution).

**Performer-gesture line:** progress driven by a saw LFO loops the clip cleanly at the wrap point (§3, restated) — this is gate item 3 above, not optional.

## A2 Phase Brief (written 2026-07-16, orchestrating session, re-derived inventory)

**Entry state:** `origin/main` HEAD `87b803fd` (A1 SHIPPED). Test fixtures present:
`tests/fixtures/gltf/khronos/{CesiumMan,Fox,BrainStem,RiggedFigure,RiggedSimple}.glb`.
`CesiumMan`/`Fox` are the doc's named A2 gate fixtures (§3); `BrainStem` was named as
"the joint-count stress case, re-derive its joint count" — re-derived below, and it
turned out to be the WRONG stress axis (see Deviation from D2 below).

**Re-derived / re-confirmed inventory:**
- `MeshData` doesn't exist as a named type (§1's audit note was right to flag this
  generically) — the real vertex type is `crates::generators::mesh_common::MeshVertex`
  (48 bytes, fixed position/normal/uv layout), used pervasively. D2's "MeshData grows
  optional JOINTS_0/WEIGHTS_0 attributes" is resolved the way `node.morph_mesh`'s
  `weights` input already proves: **two new SEPARATE coincident `Array` inputs**
  (`joints`, `weights`, both `Array(Vec4Vertex)` — the existing `[f32;4]`-shaped
  KnownItem, reused rather than adding a joints/weights-specific type), not a layout
  change to `MeshVertex` itself. "One owner" is satisfied because
  `node.gltf_skinned_mesh_source` is the sole writer of the coincident joints/weights
  buffers, the same way `node.gltf_mesh_source` is the sole writer of `MeshVertex`
  buffers today.
- No joint-matrix-palette `KnownItem` existed. Added `generators::mesh_common::JointMatrix`
  (4×`Vec4F` columns, 64 bytes, column-major — byte-identical to `gltf_load::Mat4`) with
  new well-known channel names `mat_col0..3` (`channel_names.rs`). `InstanceTransform`
  (TRS-decomposed) was confirmed NOT suitable, per the pre-flight note — a joint's skin
  matrix (`jointWorldMatrix * inverseBindMatrix`) is a general affine 4×4, not guaranteed
  TRS-decomposable.
- `gltf_load.rs` had zero `skins()`/JOINTS_0/WEIGHTS_0 parsing (confirmed, matches the
  design doc's audit). Added: `GltfSkinInfo` (per-skin topology: joint node indices, each
  joint's parent WITHIN the joint list or `-1`, the static world transform of whatever
  lies ABOVE the joint tree for root joints, inverse-bind matrices, and each joint's
  static BIND-pose TRS via `node.transform().decomposed()`), `parse_skins`,
  `flatten_skinned_node` (reads JOINTS_0/WEIGHTS_0 via the `gltf` crate's
  `read_joints(0).into_u16()` / `read_weights(0).into_f32()`), `load_gltf_skinned_mesh`.
  `GltfMaterialInfo` grows `skin: Option<GltfObjectSkin>`, resolved under the IDENTICAL
  single-node-per-material scope boundary A1's `animation` field already uses.
- **Real deviation from D2, found by rendering not assumed:** a skinned mesh's
  positioning comes ENTIRELY from the joint hierarchy — glTF 2.0 §3.7.3.3 says the
  mesh-owning node's own transform is ignored for a skinned mesh. The existing
  `node.gltf_mesh_source` Material selector WORLD-TRANSFORMS vertices by the
  contributing node's own bind matrix (correct for every static/rigid object, wrong for
  a skinned one — would double-transform). Rather than retrofit that primitive's
  extensive staging/background-thread machinery with a skinning-aware transform bypass,
  A2 ships a sibling primitive, `node.gltf_skinned_mesh_source`, that never applies a
  node transform at all and emits three coincident array outputs
  (`vertices`/`joints`/`weights`) from one background-thread parse — same pattern,
  narrower scope, zero risk to the ~185-primitive-wide existing mesh-source path.
- **Skeleton pose sampling, resolved simpler than the design doc's "walk the whole
  document's parent chain" framing implied:** rather than threading generic
  per-NODE Tables through the primitive (which would need a document-wide parent map
  + a heterogeneous static-vs-animated representation per node), `node.gltf_skeleton_pose`
  takes six flat Tables keyed by JOINT index (not node index) —
  `joint_parent_table`, `joint_root_world_table`, `inverse_bind_table`,
  `translation_tracks`/`rotation_tracks`/`scale_tracks` — built once at import time by
  `gltf_import.rs::build_skeleton_pose_tables`. Every joint ALWAYS has a translation/
  rotation/scale row group: an animated joint gets its real multi-keyframe track (from
  clip `[0]`'s per-node map, reused directly — `GltfImportSummary.animations`'s
  `#[allow(dead_code)]` is removed, it's now a real consumer); an unanimated joint gets a
  SINGLE static row from its own BIND pose (`GltfSkinInfo::joint_bind_translation/
  _rotation/_scale`) — this is why A2 does NOT reuse A1's "unanimated channel defaults to
  identity" convention: a joint's rest pose is very often non-identity (an elbow bend, a
  T-pose offset), and skinning has no OTHER source for that offset the way a rigid
  object's baked static vertex positions do. `node.gltf_skeleton_pose::run()` reuses the
  IDENTICAL binary-search+lerp/slerp sampler A1's `gltf_animation_source` proved, just
  against per-joint row RANGES inside the flat Tables (one linear scan per joint per
  frame to find the range — cheap at the joint counts these fixtures carry, confirmed by
  the hot-path gate below) instead of one Table per channel.
- **§2.5 audit (CLAUDE.md, mandatory before proposing `node.skin_mesh`):**
  `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"` — no
  existing primitive does per-vertex joint blending, matrix-palette lookup, or anything
  adjacent (`node.morph_mesh` is the nearest relative — a coincident two-mesh lerp with
  an optional coincident weights buffer — and it directly informed the `joints`/`weights`
  input shape above, but has no matrix-palette concept at all). Genuinely new primitive,
  confirmed.
- **Codegen path (mandatory, CLAUDE.md standing rule):** `node.skin_mesh` ships with
  `fusion_kind: Pointwise`, three COINCIDENT array inputs (`in: MeshVertex`,
  `joints`/`weights: Vec4Vertex`) plus one `BufferGather` array input (`matrices:
  JointMatrix` — a joint-index lookup, NOT coincident with the per-vertex dispatch,
  the same access kind `node.neighbor_smooth`/`node.tube_from_path` already use for a
  body-computed-index buffer read). Empirically verified (not guessed) which synthesized
  WGSL struct names the codegen assigns each distinct Channels signature by printing
  `standalone_for_spec::<SkinMesh>()`'s actual output before finalizing `skin_mesh_body.wgsl`
  — `Element` (MeshVertex), `Element2` (Vec4Vertex, shared by both `joints` and
  `weights` since they're the same KnownItem), `Element3` (JointMatrix). `run()` builds
  its pipeline from `standalone_for_spec::<Self>()`, never a hand `include_str!` runtime
  kernel.

**Deliverables (all shipped):**
1. `gltf_load.rs`: `GltfSkinInfo`/`GltfObjectSkin` + `parse_skins`/`flatten_skinned_node`/
   `load_gltf_skinned_mesh` (skin topology, JOINTS_0/WEIGHTS_0, inverse-bind matrices,
   parent-within-joint-list resolution, static root-world composition for joints whose
   real parent lies outside the joint tree).
2. `node.gltf_skinned_mesh_source` (`primitives/gltf_skinned_mesh_source.rs`) — bind-pose
   local-space vertices + coincident joints/weights, background-thread parse + per-frame
   blit, sibling of `node.gltf_mesh_source`.
3. `node.gltf_skeleton_pose` (`primitives/gltf_skeleton_pose.rs`) — CPU-only
   (`boundary_reason: NonGpu`), samples six flat per-joint Tables at a live `progress`
   (same D3 default beat-drive as A1), composes parent-chain world matrices (memoized,
   cycle-guarded), emits `Array(JointMatrix)`.
4. `node.skin_mesh` (`primitives/skin_mesh.rs`) — codegen-path GPU linear-blend skinning,
   4-joint weighted blend (weights normalized defensively), out-of-range joint indices
   clamp rather than read out of bounds. Mandatory generated-vs-hand parity test: since
   this is a brand-new primitive with no legacy predecessor, "hand" is an
   independently-implemented Rust reference of the committed formula
   (DECOMPOSING_GENERATORS.md §9's documented convention for exactly this case), not a
   parallel `.wgsl` file.
5. `gltf_import.rs::build_import_graph`: when `GltfMaterialInfo::skin` resolves, an
   object's group wires `node.gltf_skinned_mesh_source` → `node.skin_mesh` (`in`/`joints`/
   `weights`) with `node.gltf_skeleton_pose`'s `joint_matrices` feeding `skin_mesh.matrices`,
   and the group's `vertices` output interface wires from `skin_mesh.out` instead of the
   mesh source directly — replacing (not augmenting) the static `node.gltf_mesh_source`
   path for that object, since a skinned object's rest geometry alone is never what
   should render.

**Gate (all passed):**
- Positive: `node_graph::gltf_import::tests::skinned_characters_render_four_visibly_distinct_deformed_poses`
  (`--features gpu-proofs`) — `CesiumMan.glb` and `Fox.glb` each render four
  progress-swept frames that are pairwise byte-distinct AND, on visual inspection
  (per CLAUDE.md's "a green test is not a look" — actually opened the PNGs), show a
  correctly-textured running character in visibly different limb/stride phases, not
  noise. Goldens: `tests/fixtures/gltf/goldens/{cesiumman,fox}_skin_p0{00,25,50,75}.png`.
- Parity: `node_graph::primitives::skin_mesh::gpu_tests` (`--features gpu-proofs`) — 3/3
  green (single-joint full-weight, two-joint blend, out-of-range-index clamp), generated
  kernel vs. the independent Rust reference.
- Hot-path: `node_graph::gltf_import::tests::skinned_import_hot_path_stays_under_20ms_per_frame`
  (`--features gpu-proofs`) — substitute for the `MANIFOLD_RENDER_TRACE`-driven
  `manifold-app` journey-proof harness (wiring a full content-thread project/layer/
  generator around an imported glTF asset is real additional infrastructure this phase
  didn't build); measures actual GPU encode-to-submit wall time on a warm `PresetRuntime`,
  30 frames after a 10-frame warmup. `CesiumMan.glb`: avg 6.0ms, max 7.5ms. `Fox.glb`:
  avg 5.4ms, max 5.8ms. Both comfortably under the 20ms budget.
- Negative gate carried over from A1: scope stayed inside A2 — no morph-target work (A3),
  no clip-selector/performance-surface work (A4).
- Test scope: `cargo test -p manifold-renderer --lib` (default sweep, 1339/1339 green) +
  `cargo test -p manifold-renderer --features gpu-proofs --lib` (targeted skin_mesh/
  gltf_skeleton_pose/gltf_skinned_mesh_source/gltf_import module runs, all green) +
  `cargo run -p manifold-renderer --bin gen_node_catalog` (regenerated for the 3 new
  primitives — `node_graph::catalog_gen::tests::regenerates_in_sync` requires this after
  any primitive addition) + `cargo clippy -p manifold-renderer -- -D warnings` (clean,
  worktree-scoped).

**Deviation found and NOT chased (logged instead):** `BrainStem.glb`, the design doc's
named "joint-count stress case," turned out to actually be a MANY-SMALL-SKINS case — 24
separate skinned objects sharing an 18-joint skeleton (not one big joint palette).
Rendering it measured a flat ~370ms/frame from frame 0 on the identical hot-path harness
that measures CesiumMan at 6ms — an 18x-over-budget number worth taking seriously, but
NOT diagnosed this session (BrainStem was never a named A2 gate fixture; the actual gate
fixtures pass cleanly) and NOT something to guess-fix under session time pressure per
DESIGN_AUTHORING.md's discipline. Logged as **BUG-190**, cross-referenced against
BUG-189 (a same-shaped "many-material glTF import has a large resolution-independent
per-frame GPU floor" finding from the same day) since they may share a root cause.

**Forbidden moves (all held):** CPU skinning (D2, decided — `node.skin_mesh` is GPU,
codegen-path, no plain-WGSL fusion boundary); a fused single-effect/generator monolith
(the three primitives stay single-dispatch/single-CPU-op each, composed in the graph);
scope-creep into A3 (morph targets) or A4 (clip selector, performance surface, retrigger).

## 5. Deferred
- Clip blending/crossfade (D4) · animation-pointer property targets (D5) · IK/retargeting (never in scope for import) · timeline-clip integration (an imported animation as a timeline clip with in/out points — real feature, but it builds ON A1–A4's progress param; trigger: Peter's call after playing with A4).
