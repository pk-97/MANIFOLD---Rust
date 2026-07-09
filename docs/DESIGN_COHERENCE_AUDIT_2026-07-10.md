# Cross-Design Coherence Audit — running ledger

**Status: IN PROGRESS · 2026-07-10 · Fable (fresh session, per the approved brief)**
**Scope: every board entry APPROVED-not-built or IN PROGRESS as of 2026-07-10 (board regenerated this session). 36 designs.**

Method (per brief): build-order completeness check → cluster by seams → per-design ledger
(surfaces claimed · amends/assumes · prerequisites · sampled code anchors) → per-cluster
collision hunt → kill-pass each finding against the second doc's actual text.

This file is the survival mechanism across context summarization — it is committed
incrementally and each section is self-contained. Final outputs (conflict list, per-design
verdicts, build-order update, Opus-vs-mechanical handoff split) accrete at the top once
clusters complete.

---

## 0. Build-order completeness check (brief step 1)

`DESIGN_BUILD_ORDER.md` §1 claims to cover "every APPROVED-not-built design doc." Checked
against the regenerated board 2026-07-10:

**Missing entirely (approved 2026-07-08/09, never added):**
- VIDEO_IO (approved 2026-07-09)
- UI_HARNESS_UNIFICATION (approved 2026-07-09)
- PERF_BUDGET_GATE (approved 2026-07-09)
- LIVE_RECORDING_PROOFS (approved 2026-07-09, release-gating per STRUCTURAL_AUDIT_VERDICTS)
- BOX3D_PHYSICS (approved 2026-07-09)
- AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION (approved 2026-07-09)
- RENDER_SCENE_UNBOUNDED_LIGHTS (approved 2026-07-06)
- AUDIO_OBJECT_TRACKING (approved 2026-07-06)
- AUDIO_OBJECT_INGEST (approved direction 2026-07-06)
- KICK_SWEEP_EVENT (IN PROGRESS — remaining phases unsequenced)
- (directions, arguably pre-queue: MAPPING_GRAMMAR, AUTO_POPULATE — noted, not counted as findings)

**Present but shipped (should be removed per §5 maintenance):**
- AUTOMATION_LANES (P1–P4 shipped; P5 partial — check whether a row remnant is warranted)
- MATERIAL_SYSTEM (M1–M6 shipped — row already annotated but still present)
- OVERLAY_SESSIONS_AND_PICKER (P1–P2 shipped)
- PRESET_LIBRARY (P0–P6 shipped)
- TIMELINE_INGEST (P3+P4+P5 shipped; P1/P2 parked on BUG-028 — remnant row may be warranted)
- PARAM_STORAGE_BOUNDARIES (P1–P3 shipped 2026-07-09)
- PARAM_STORAGE row says "P1–P5 SHIPPED" but board says IN PROGRESS — reconcile with doc.

**Stale annotations spotted (verify before fixing):**
- §3 item 4: "MULTI_DISPLAY P2 ⏸ BLOCKED pending §6.1 seam-hardening" — board says §6.1a
  resolved 2026-07-06. Check DESIGN_HARDENING_QUEUE item 2.
- §3 item 6: "MEDIA_BACKEND P1 ⏸ PARKED (§3 addendum needed)" — board says §3a addendum
  resolved 2026-07-06. Check DESIGN_HARDENING_QUEUE item 1.

## 1. Cluster assignment (initial; refined by reading seams, not titles)

- **3D + render_scene:** REALTIME_3D · SCENE_BUILD_AND_GROUP_PARAMS · RENDER_SCENE_UNBOUNDED_LIGHTS · SIMULATIONS · BOX3D_PHYSICS · GAUSSIAN_SPLATS · IMPORT · ML_NODES
- **Perform path:** SESSION_MODE · PERFORM_SURFACE · MULTI_DISPLAY · GIG_RESILIENCE · APP_SHELL · DJ_PERFORMANCE · PRO_DJ_LINK · ABLETON_SHOW_SYNC
- **Audio:** KICK_SWEEP_EVENT · AUDIO_ANALYSIS_ACCURACY · AUDIO_OBJECT_TRACKING · AUDIO_OBJECT_INGEST · AUDIO_SETUP_DOCK · MAPPING_GRAMMAR · AUTO_POPULATE
- **Param/card:** PARAM_STORAGE · COMPONENT_LIBRARY · MCP_INTERFACE (+ SCENE_BUILD P3 seam)
- **io/export/media:** VIDEO_IO · MEDIA_BACKEND · COMMERCIALIZATION · LIVE_RECORDING_PROOFS
- **LED/output:** LED_STRIPS · PROJECTION_MAPPING (+ MULTI_DISPLAY seam)
- **Dev infra:** UI_AUTOMATION · UI_HARNESS_UNIFICATION · PERF_BUDGET_GATE · VULKAN_BACKEND

(Designs appearing in two clusters get audited in both — cluster membership is a lens,
not a partition.)

---

## Per-design ledger

(filled per cluster; format: surfaces claimed · amends/assumes · prereqs · anchors sampled)

### Cluster: 3D + render_scene (COMPLETE — all 8 docs read whole 2026-07-10)

- **REALTIME_3D** — surfaces: `render_scene.rs`, light.rs, camera atoms, viewport/gizmos (P5/P6), atmosphere port (P3), shadows (P2). Amended by SCENE_BUILD §8 (D3/D8/P6 — annotated in status line, coherent). Anchors: `OBJECT_SLIDER_MAX=64` shipped (objects UNCAPPED on main); `MAX_LIGHTS=4` still in code (UNBOUNDED_LIGHTS unbuilt).
- **SCENE_BUILD_AND_GROUP_PARAMS** — surfaces: render_scene rebuild/evaluate, `PortType::Transform`, `node.transform_3d`, migration chain (v1120), gltf_import.rs, ParamSpecDef.section, param_card.rs, graph_canvas rows/ribbons, AddSceneObjectCommand. Prereq: BOUNDARIES P1–P2 (SHIPPED 2026-07-09) for P3 only — prereq now satisfied, doc header still says "must land before P3" as if future. Anchors: `MAX_RENDER_SCene_OBJECTS=8` still in gltf_import.rs:49 (P2's fix still owed) ✓.
- **RENDER_SCENE_UNBOUNDED_LIGHTS** — surfaces: render_scene.rs + render_scene.wgsl only. Claims "independent of everything else in flight" — textually true, but same file/functions as SCENE_BUILD P2. Anchors: MAX_LIGHTS/LIGHT_NAMES/:112 uniform all verified present ✓.
- **SIMULATIONS** — surfaces: new solver atoms, array_feedback state. Prereq REALTIME_3D P1 (shipped ✓). §8 rigid-body deferral carries the BOX3D supersede note (cross-patched 2026-07-07 ✓).
- **BOX3D_PHYSICS** — surfaces: new `manifold-physics` crate, PortType::BodySet/ColliderSet, `node.physics_world`, render_copies (consume only). Supersede of SIMULATIONS §8 recorded on BOTH sides ✓. Collider-vocab reconciliation explicitly deferred to SIMULATIONS §2.5 audit ✓. Clean pair.
- **GAUSSIAN_SPLATS** — surfaces: Splat channel struct, gpu_sort.rs (new shared infra), render_splats, render_scene lazy `depth` output (P4). Anchors: free_camera/look_at_camera pinned landed ✓ (consistent with REALTIME_3D status).
- **IMPORT** — surfaces: manifold-io importers, node.placeholder, node.mesh_sequence, session grid (P5), MCP (P6). Prereqs M6 + REALTIME_3D P1 (both shipped ✓). Reality note (2026-07-05) describes gltf wave as "single-mesh source" — see F5.
- **ML_NODES** — surfaces: manifold-native inference module, node.camera, Keypoint2D array, model cache. Prereq: ONNX tier needs VULKAN ✓ (in build order). Seams to verify in later clusters: MULTI_DISPLAY §12 calibration; VIDEO_IO vs its deferred "NDI/Syphon in, same slot later" source claim.

**Cluster findings (kill-passed against second doc's text + code):**
- **F2 (HIGH) — REALTIME_3D P2 shadows is priced against caps that no longer exist.** D3/D4/§6/§7.3 assume "8 objects, 4 lights; the cap exists so the worst case stays bounded" (40 geometry passes). Shipped code: objects=64. Approved sibling UNBOUNDED_LIGHTS: lights→64 soft/127 hard. Worst case becomes 64 objects × 64 casters ≈ 4096 geometry passes; shadow-map cache keyed (node, light_slot) inflates likewise. Neither sibling amends the shadow story (UNBOUNDED_LIGHTS scope excludes it; shadows unbuilt). **REALTIME_3D must be amended before P2 executes**: caster budget/policy (e.g. cast_shadows honored for first-K lights, K a constant) + strike the stale cap text in D3/§7.3. Opus-level (a real cost-policy decision).
- **F3 (MED) — three unbuilt designs edit `render_scene.rs` with no cross-references**: SCENE_BUILD P2 (rebuild/evaluate rewrite), UNBOUNDED_LIGHTS P1 (uniforms+shader+rebuild), SPLATS P4 (lazy depth output). Any pairwise order works, but each doc's audit anchors break when a sibling lands first, and UNBOUNDED_LIGHTS claims "independent of everything in flight" while SCENE_BUILD P2's gate asserts "if any .wgsl diff appears in this phase, stop" (correct today; still correct after UNBOUNDED_LIGHTS since that phase touches Rust-side only — but the executor needs to know which shader baseline they're on). Fix: BUILD_ORDER gains a same-file sequencing note; UNBOUNDED_LIGHTS prereq line names the co-claimants. Mechanical.
- **F4 (MED) — GAUSSIAN_SPLATS D10 cites a convention SCENE_BUILD deletes.** D10: placement = "TRS params on render_splats, port-shadowed — same convention as `render_scene` object groups". SCENE_BUILD D3/Decided-1: that convention dies ("per-object transforms are graph structure, not renderer params"; params deleted). Splat placement as node params remains defensible (single object ≈ render_mesh's own TRS, which SCENE_BUILD §9 leaves alone), but: (a) the cited precedent must be re-anchored to render_mesh/render_copies; (b) decide param-TRS vs optional `transform: Transform` input — without the port, splat placement is invisible to REALTIME_3D P6 gizmos; (c) SCENE_BUILD §9's deferred "Transform port on render_mesh/render_copies" list should name render_splats when it exists. (a)/(c) mechanical; (b) one Opus judgment line.
- **F5 (MED-HIGH) — IMPORT's reality note mischaracterizes what shipped; P1 as written re-builds existing code.** Note (2026-07-05) says the glTF wave shipped "a single-mesh source primitive." As-built `gltf_import.rs::build_import_graph` (fb97c6a2 + successors, verified in-tree) assembles a full render_scene graph: per-material named+tinted object groups, camera rig + sun, curated 13-slider card, recenter, over-cap warning. That IS most of IMPORT P1's deliverable in different form (missing: import report doc, single undo transaction, Khronos conformance fixtures, manifold-io placement per D1/§3). SCENE_BUILD D9 amends this importer further. **IMPORT needs a pre-P1 reality-note refresh + P1 scope re-cut** (what's left: report, undo-transaction wrapper, conformance, hierarchy pre-compose, light mapping) — otherwise the P1 executor rebuilds or forks the shipped importer. Scope re-cut = Opus; note refresh = mechanical.
- **F6 (LOW) — IMPORT D1 over-cap text stale**: ">8 objects" → importer cap is 8 today only as a stale mirror; renderer is 64; SCENE_BUILD D9 makes the importer threshold `OBJECT_SLIDER_MAX` (64). Mechanical text fix, fold into F5's refresh.
- **F-clean:** SIMULATIONS↔BOX3D mutual supersede coherent; SPLATS P4 lazy-depth matches REALTIME_3D §3's promise; SPLATS↔CHANNEL_TYPE_SYSTEM consistent; SCENE_BUILD prereq on BOUNDARIES now satisfied (update header phrasing when amending).

---

## Conflict list

(accretes; each entry: severity · the two docs · one-line collision · which doc must change)

---

## Per-design verdicts

(clean / needs amendment (named) / stale anchors (named))

---

## Handoff split

(Opus amendment session vs mechanical fixes)
