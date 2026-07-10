# Cross-Design Coherence Audit — running ledger

**Status: COMPLETE · 2026-07-10 · Fable (fresh session, per the approved brief). All 7 clusters audited; 19 findings kill-passed; conflict list + verdicts + handoff split final; DESIGN_BUILD_ORDER updated in the same landing.**
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

### Cluster: perform path (COMPLETE — all 8 docs read whole 2026-07-10)

- **SESSION_MODE** — surfaces: session.rs model, SessionRuntime (engine third ref source), session_commands.rs, grid panel (P4) + ContentState session fields (greenfield, P4). Cross-refs PERFORM_SURFACE P2 pairing ✓ both sides.
- **PERFORM_SURFACE** — surfaces: perform_mode/* (chrome-hosting rewrite, P1 deletes hand-drawn path), SurfaceDef/widget registry, chrome API. P2 = SessionGrid widget.
- **MULTI_DISPLAY** — surfaces: stage.rs, layer_compositor chain maps (P2 seam table §6.1a), content_pipeline present path (P3), StageUniform + 3 atoms (P4), stage view UI (P5), LED/DMX/MVR/NDI/Syphon (P6). §5a ONE-Stage-surface amendment claims matching addenda in PROJECTION_MAPPING/LED_STRIPS/APP_SHELL — APP_SHELL §8 verified ✓; other two checked in LED cluster.
- **GIG_RESILIENCE** — surfaces: autosave/alerts (P1 ✓), breadcrumb/--resume (P2 ✓), manifold-understudy crate + perform arming (P3), peripherals (P4). P3's visible widgets = PERFORM_SURFACE §7.0 ✓ both sides.
- **APP_SHELL** — surfaces: commands.rs table, menu.rs rebuild, two settings windows, AppPrefs. §8 = the slot-contract reconciliation table (2026-07-06) — the strongest cross-doc coherence artifact in the corpus.
- **ABLETON_SHOW_SYNC** — surfaces: manifold-io/src/als/, AbletonRef/ImportState, CueMarker store, trigger_velocity on TimelineClip, merge command. OSC bridge explicitly unchanged.
- **DJ_PERFORMANCE / PRO_DJ_LINK** — symmetric pair (D5↔D3 master-tempo mental model, stated on both sides ✓); both post-v1.0, prereqs match build order. PRO_DJ_LINK's "sync-source seam" prereq is now partially SHIPPED (ABLETON_TRANSPORT_SYNC SyncArbiter, 2026-07-07) — anchor refresh at brief time, not a conflict.

**Cluster findings (kill-passed):**
- **F7 (MED) — PERFORM_SURFACE still commits to a "Perform button" that APP_SHELL D4 (Peter, verbatim, 2026-07-06) forbids.** PERFORM_SURFACE D1 + Decided-#1 + P2 ("Perform-button context routing"): "One Perform button... No setting." APP_SHELL D4/R5 + §1 audit: perform entry is menu-only, "no header/transport PERFORM button, no accelerator" — and as-built header has no mode buttons. APP_SHELL §8 records that it decided the affordance; PERFORM_SURFACE carries no amendment. D1's surviving content (entry follows context: arrangement → timeline perform, session view → session perform) moves onto the menu item's routing. **PERFORM_SURFACE must change** (D1, Decided-#1, P2 wording). Mechanical.
- **F8 (MED) — GIG_RESILIENCE's D8 caveat was invalidated by a shipped landing.** Header caveat (2026-07-03): "the bridge (`ableton_bridge.rs:250`) has no inbound song-position field... deferred to ABLETON_SHOW_SYNC... decision pending Peter." As-built 2026-07-07 (ABLETON_TRANSPORT_SYNC P1–P3): `song_time` inbound listener exists (`ableton_bridge.rs:242, :891, :2305`). The Ableton-position rejoin branch of `--resume` is now buildable; the deferral pointer aims at the wrong doc (transport belongs to ABLETON_TRANSPORT_SYNC, which shipped it). **GIG_RESILIENCE must change** (header caveat truth-fix; remaining work = wiring --resume rejoin to the shipped listener, a P3/P4-adjacent task). Mechanical + one Peter decision line (build the rejoin branch or drop it).
- **F9 (LOW) — two deferral triggers have FIRED without the docs noticing:** (a) ABLETON_SHOW_SYNC D16 defers automation-envelope import "once AUTOMATION_LANES is built" — lanes shipped P1–P4 (2026-07-04); the executing session should decide fold-in vs keep-deferred at brief time. (b) ABLETON_SHOW_SYNC §9 defers session-view scenes "after SESSION_MODE ships" — P1–P3 shipped; the SessionLaunchScene id-mapping SESSION_MODE §5 punts to "the Ableton-sync project" is designed NOWHERE. Note in both briefs; no doc contradiction.
- **F-clean:** SESSION_MODE↔PERFORM_SURFACE pairing coherent both directions · GIG_RESILIENCE§7↔PERFORM_SURFACE§7.0 coherent · MULTI_DISPLAY§5a↔APP_SHELL§8 coherent · DJ_PERFORMANCE↔PRO_DJ_LINK symmetric · APP_SHELL D6↔MCP_INTERFACE catalog rule coherent (verify MCP side in param cluster).
- **Build-order sharpening (fold into update):** PERFORM_SURFACE P2's true prereq is SESSION_MODE **P4's ContentState plumbing** (the snapshot fields are greenfield P4 work per SESSION_MODE baseline review), not just P1–P3.

### Cluster: audio (COMPLETE — all 7 docs read whole 2026-07-10)

- **KICK_SWEEP_EVENT** — IN PROGRESS; P1/P2/P4/P5 shipped, P3 feel-pass owed to Peter. Supersessions of AUDIO_OBJECT_TRACKING D9/P6 recorded on BOTH sides ✓. Own Decided items 6/7 properly struck-through when superseded — the corpus's best example of in-doc truth maintenance.
- **AUDIO_ANALYSIS_ACCURACY** — surfaces: tools/audio_analysis/eval/ (new harness), bpm.py (madmom deletion), Rust seams P5 (PercussionTriggerType variants, parser, planner clamp, onset_compensation→0). Companions named: CLIP_DETECTION, OBJECT_TRACKING, CHANNEL_TYPE_SYSTEM — **not OBJECT_INGEST** (see F10).
- **AUDIO_OBJECT_TRACKING** — P1–P4 SHIPPED in-body (2026-07-06) but header/board still say "not built" (F11). P5 (scope overlay) + BUG-045 remain; P6 dead → KICK_SWEEP.
- **AUDIO_OBJECT_INGEST** — conformance direction; prereq OBJECT_TRACKING P0–P2 (now satisfied); P1 blocked on Peter (labeled clips). §8 relation contract mirrored both sides ✓.
- **AUDIO_SETUP_DOCK** — surfaces: ScreenLayout (new column), audio_setup_panel, OverlayId deletion, Layer.clip_triggers, live_trigger.rs, drawer builder. Declares supersessions of AUDIO_SENDS D6/§3.3 + APP_SHELL §7 — one-sided (F12). Owns BUG-047/070/082 ✓ matches memory.
- **MAPPING_GRAMMAR / AUTO_POPULATE** — directions, mutually referenced, coherent with ANALYSIS_ACCURACY's 2026-07-09 addendum ✓. AUTO_POPULATE gated on BUG-069 rework ✓.

**Cluster findings (kill-passed):**
- **F10 (MED-HIGH) — ANALYSIS_ACCURACY and OBJECT_INGEST claim the same measurement + synth-stem surface with unreconciled doctrines and no cross-reference.** OBJECT_INGEST P1 builds "a scoring script (Python, beside the pipeline)" with Peter-labeled clips as ground truth and D4 replaces basic_pitch per-instrument on that measurement. ANALYSIS_ACCURACY (2 days newer) builds the full eval/ harness with frozen metrics (D10), dataset-only tuned sets (D9), Peter material "never the tuned sets" (Deferred #5) — and its P4 TUNES basic_pitch post-processing on the same synth/pad stems, while §4.3 builds chord clustering ON basic_pitch output that OBJECT_INGEST may remove from those stems. Reconcilable (a one-shot component measurement isn't a tuning loop) but as written two Sonnet waves would build two harnesses and contest one detector surface. **OBJECT_INGEST must be amended** (P1 consumes eval/ as a fixture role instead of a bespoke script; D4's replacement decision sequenced after ANALYSIS_ACCURACY P3 baselines; chord-emitter interplay stated); ANALYSIS_ACCURACY adds OBJECT_INGEST to companions + one sequencing line. Opus-level (measurement-doctrine reconciliation).
- **F11 (MED) — AUDIO_OBJECT_TRACKING's status header is false, so the board is false.** Header: "APPROVED design, not built · 2026-07-06." Body: P1 ✅, P2 ✅ (all gates green), P3 ✅, P4 ✅ SHIPPED (`586d2bac` + fix `00e9fd19`) — all 2026-07-06. Remaining: P5 scope overlay, BUG-045, P6 dead. The board is the sole status source (memory doctrine), and it currently under-reports a mostly-shipped design. Mechanical header truth-fix + board regen.
- **F12 (MED) — AUDIO_SETUP_DOCK's amendments are one-sided.** It amends APP_SHELL §7 (Audio Setup T2 modal → T1 workspace surface; Decided-#9 "stays a modal" now false), supersedes AUDIO_SENDS_UX D6/§3.3, and outdates AUDIO_INFRASTRUCTURE §7's "stays modal" — none of the three carry a back-reference. A reader of APP_SHELL §7/§8 or Decided-#9 gets the dead classification with no pointer. Mechanical: supersession lines in all three docs.
- **F13 (LOW) — OBJECT_TRACKING P5's anchors predate the ScopeColumn typed-overlay refactor** (KICK_SWEEP's scope lane landed on it 2026-07-07, `b6aed008`). `SCOPE_SCALAR_STRIDE = 7` and the shader-overlay shape may have moved. Re-derive at P5 brief; standard anchor-refresh, noted because the doc's stride plan (7→11) is load-bearing.
- **F-clean:** KICK_SWEEP↔OBJECT_TRACKING supersessions mirrored ✓ · OBJECT_INGEST↔OBJECT_TRACKING relation contract mirrored ✓ · AUDIO_SETUP_DOCK↔KICK_SWEEP Kick-feature story consistent ✓ · MAPPING_GRAMMAR↔AUTO_POPULATE↔ANALYSIS_ACCURACY addendum chain coherent ✓.

### Cluster: param/card (COMPLETE 2026-07-10 — COMPONENT_LIBRARY + MCP read whole; PARAM_STORAGE status/prereq/companions read, body deliberately skipped: P1–P5 shipped, sole remainder is Peter's library re-save, no unbuilt cross-design seam)

- **PARAM_STORAGE** — effectively shipped; verdict clean-with-remainder (Peter-owned re-save). Board status "IN PROGRESS" is accurate.
- **COMPONENT_LIBRARY** — surfaces: ComponentDef/registry (manifold-core), GroupParamDef extensions (§4a), BindingDef.extra_targets, component picker (after node-groups canvas). Prereq VOCAB apply ✓ shipped. MCP §5 amendment mirrored on BOTH sides ✓.
- **MCP_INTERFACE** — surfaces: new manifold-mcp crate, mcp channel into content thread, 14 tools, MoodBoard model on Project. APP_SHELL D6 command-table slot coherent (forward reference, one-way by design) ✓. MULTI_DISPLAY §7.2 stage-summary note additive ✓.

**Cluster finding (kill-passed):**
- **F14 (MED) — SCENE_BUILD declares "GroupParamDef stays unused" while COMPONENT_LIBRARY's macro layer is built on it.** SCENE_BUILD §1 audit row + Decided-#3 (2026-07-06): Phase D dead, "GroupParamDef stays unused," card bundling = ParamSpecDef.section; §9 defers group interface params "for swappable 'rack' presets with stable knob surfaces." COMPONENT_LIBRARY (2026-07-02) §4a extends GroupParamDef (fan-out targets, labels, ranges) as the component macro schema — and its §9-deferred use case IS components, unnamed. The two compose mechanically (macros are declarations that lower onto ordinary card BindingDefs at expose — no live group-param runtime, so SCENE_BUILD's actual kill target stays dead), but the texts contradict on the type's future and neither cites the other. Amend both: SCENE_BUILD names COMPONENT_LIBRARY as the sanctioned GroupParamDef consumer (declaration-only, lowered to the card path); COMPONENT_LIBRARY notes card sections (ParamSpecDef.section) are orthogonal. Mechanical once the compose-check is confirmed (one Opus/Fable sentence).

### Cluster: io/export/media (COMPLETE — all 4 docs read whole 2026-07-10)

- **VIDEO_IO** — surfaces: manifold-native Syphon/NDI FFI, VideoSendDef in stage.rs/venue file, node.syphon_in/ndi_in source atoms, content-pipeline publish seam. Claims MULTI_DISPLAY P6's "NDI/Syphon outputs" item as its own — correctly, with citation ✓. License clearance §0 thorough (Opus-read NDI agreement).
- **MEDIA_BACKEND** — §3a addendum resolved (hardening queue item 1 ✓); P1 RE-ISSUABLE. Superseded §3 trait kept for the record ✓ exemplary truth maintenance.
- **COMMERCIALIZATION** — P4 rides GIG_RESILIENCE P1–P2 (shipped ✓). PANNs CC BY 4.0 risk note present, matching ANALYSIS_ACCURACY's addendum claim ("logged in COMMERCIALIZATION_DESIGN") ✓.
- **LIVE_RECORDING_PROOFS** — (read in dev-infra pass below.)

**Cluster findings (kill-passed):**
- **F15 (LOW-MED, mechanical) — NDI/Syphon ownership moved to VIDEO_IO; two docs still point at the old owner.** MULTI_DISPLAY §10 P6 still lists "NDI/Syphon outputs" as its own future work; MEDIA_BACKEND's Deferred line says "NDI/Syphon (multi-display §10 P6 owns those)"; ML_NODES §4/§14 defer "NDI in, Syphon in — same slot later" with no forward pointer. VIDEO_IO (2026-07-09) is now the owner and says so. Cross-patch three one-liners (MULTI_DISPLAY P6, MEDIA_BACKEND deferred, ML_NODES §14) → VIDEO_IO_DESIGN.
- **F16 (folded into §0) — build-order §3 items 4/6 stale:** "MULTI_DISPLAY P2 ⏸ BLOCKED" and "MEDIA_BACKEND P1 ⏸ PARKED" — both hardening items resolved 2026-07-06, both phases re-issuable. Fixed in the build-order update.
- **F-clean:** VIDEO_IO↔MULTI_DISPLAY island-send trigger coherent · VIDEO_IO D3 fixture-routing unification carries its own VERIFY-AT-IMPL ✓ · COMMERCIALIZATION↔ANALYSIS_ACCURACY BUG-069 sequencing consistent (build order 13g: P2+P6 before launch-gate item 19) ✓ · MEDIA_BACKEND↔VULKAN §6/§8 pairing consistent both sides (verify VULKAN side in dev-infra pass).

### Cluster: LED/output + dev infra (COMPLETE 2026-07-10 — LED_STRIPS, PROJECTION_MAPPING, UI_HARNESS, PERF_BUDGET_GATE read whole; LIVE_RECORDING_PROOFS read through §3 (decisions+seams — remainder is test detail with no cross-doc surface); UI_AUTOMATION assessed via its shipped P1–P2 + UI_HARNESS's audit of it; VULKAN checked at status/§8/prereq lines)

- **LED_STRIPS / PROJECTION_MAPPING** — both carry the MULTI_DISPLAY §5a home amendments (LED §5a, PROJ §6) dated 2026-07-06, matching APP_SHELL §8's account exactly. The §5a cross-patch claim is fully verified across all four docs ✓.
- **UI_HARNESS_UNIFICATION** — surfaces: ui_frame.rs seam (new), UICacheManager harness path, script.rs Runner rewrite (P2), dump.rs BUG-071 fix (P0). Cites UI_CLIP_AND_Z's wrong-path PNG gate as the precedent failure ✓; pre-coordinates with LIVE_RECORDING_PROOFS on playback stepping ("coordinate, don't duplicate") ✓.
- **PERF_BUDGET_GATE** — extends trace + harness; D2 posture mirrors gpu-proofs; P2 wires GIG_RESILIENCE's checklist.
- **LIVE_RECORDING_PROOFS** — owns BUG-053 (HDR structurally broken, D7 defers behind it); injection seams zero-cost; Tier-2 = manual pre-gig recorder soak.
- **VULKAN** — background track; MEDIA_BACKEND §6 pairing consistent both sides ✓; ML ONNX gating consistent ✓.

**Cluster findings:**
- **F17 (LOW) — three docs each add a pre-gig ritual with no shared home:** GIG_RESILIENCE §10 (kill-drill), PERF_BUDGET_GATE P2 (perf soak "in GIG_RESILIENCE's checklist"), LIVE_RECORDING_PROOFS Tier 2 (recorder soak). Convergent, not conflicting — but the first to land should make GIG_RESILIENCE §10 the single checklist naming all three. Mechanical.
- **F18 (LOW) — UI_HARNESS P2 rewrites UI_AUTOMATION's shipped Runner** (deletes its parallel rebuild, re-points its render). UI_AUTOMATION's doc gains a forward note so its P3–P4 briefs re-derive Runner anchors. Mechanical.
- **F19 (LOW) — LED_STRIPS D8's H807SA UDP hardware-blackout rung isn't in GIG_RESILIENCE P3's understudy brief.** Compatible with the understudy independence rule (UDP send, no manifold deps, config-passed) but unbriefed; LED_STRIPS says "cross-reference added there at implementation" — make it a named P3-brief line item so it isn't forgotten. Mechanical.

---

## Conflict list (final — severity · docs · collision · which doc changes)

| # | Sev | Finding (one line) | Doc(s) that change |
|---|---|---|---|
| F2 | **HIGH** | REALTIME_3D P2 shadows priced against "8 objects, 4 lights" caps that shipped code (objects=64) + UNBOUNDED_LIGHTS (64/127) killed; §7.3 "do not reopen" text now false; shadow caster budget undecided | REALTIME_3D (Opus) |
| F5 | **MED-HIGH** | IMPORT's reality note calls the shipped scene-assembly importer (`build_import_graph`: named groups, camera+sun, card, cap warning) "a single-mesh source"; P1 as written rebuilds shipped code | IMPORT (Opus re-cut + mech note; fold F6 cap text 8→64) |
| F10 | **MED-HIGH** | ANALYSIS_ACCURACY and AUDIO_OBJECT_INGEST both build measurement harnesses and both claim the basic_pitch/synth-stem surface, with opposite fixture doctrines and no cross-reference | AUDIO_OBJECT_INGEST (Opus); ANALYSIS_ACCURACY (mech companion+sequencing line) |
| F7 | MED | PERFORM_SURFACE D1/Decided-1/P2 commit to a "Perform button"; APP_SHELL D4 (Peter verbatim) made entry menu-only, no button — amendment one-sided | PERFORM_SURFACE (mech) |
| F8 | MED | GIG_RESILIENCE D8 caveat ("bridge has no inbound song-position") invalidated by ABLETON_TRANSPORT_SYNC landing (`ableton_bridge.rs:242/:2305`); deferral points at the wrong doc | GIG_RESILIENCE (mech + 1 Peter line: build --resume rejoin?) |
| F11 | MED | AUDIO_OBJECT_TRACKING header says "not built" while its own body shows P1–P4 SHIPPED — the status board under-reports a mostly-shipped design | AUDIO_OBJECT_TRACKING header (mech) |
| F12 | MED | AUDIO_SETUP_DOCK amends APP_SHELL §7/Decided-9, AUDIO_SENDS D6/§3.3, AUDIO_INFRASTRUCTURE §7 — none carries a back-reference | APP_SHELL, AUDIO_SENDS_UX, AUDIO_INFRASTRUCTURE (mech) |
| F14 | MED | SCENE_BUILD "GroupParamDef stays unused" vs COMPONENT_LIBRARY §4 macro layer built on GroupParamDef; compose fine (declaration→card bindings) but texts contradict, no cross-refs | SCENE_BUILD + COMPONENT_LIBRARY (mech after 1 compose-confirm sentence) |
| F4 | MED | GAUSSIAN_SPLATS D10 cites the render_scene per-object-TRS-params convention SCENE_BUILD deletes; splat placement invisible to P6 gizmos without a Transform port | GAUSSIAN_SPLATS (mech re-anchor + 1 Opus line: params vs Transform port) |
| F3 | MED | Three unbuilt designs (SCENE_BUILD P2, UNBOUNDED_LIGHTS P1, SPLATS P4) edit `render_scene.rs`'s same functions, each claiming independence | DESIGN_BUILD_ORDER (sequencing note, this session) + UNBOUNDED_LIGHTS prereq line (mech) |
| F15 | LOW-MED | NDI/Syphon ownership moved to VIDEO_IO; MULTI_DISPLAY P6, MEDIA_BACKEND deferred-line, ML_NODES §14 still point at the old owner | those three (mech one-liners) |
| F1 | LOW-MED | DESIGN_BUILD_ORDER missing all 10 post-07-06 approvals, carries 6 shipped rows, 2 stale ⏸ annotations | DESIGN_BUILD_ORDER (fixed this session) |
| F9 | LOW | Fired deferral triggers unnoticed: ABLETON_SHOW_SYNC D16 (lanes shipped) + session-scene id-mapping designed nowhere | brief-time notes (mech) |
| F13 | LOW | OBJECT_TRACKING P5 scope-overlay anchors predate the ScopeColumn typed-overlay refactor | brief-time re-derive (mech) |
| F17 | LOW | Pre-gig ritual accretes from 3 docs with no shared checklist home | GIG_RESILIENCE §10 when first lands (mech) |
| F18 | LOW | UI_HARNESS P2 rewrites UI_AUTOMATION's shipped Runner; no forward note | UI_AUTOMATION (mech) |
| F19 | LOW | LED hardware-blackout rung absent from GIG_RESILIENCE P3 brief | GIG_RESILIENCE P3 brief (mech) |

## Per-design verdicts

**Needs amendment (named above):** REALTIME_3D (F2) · IMPORT (F5/F6) · AUDIO_OBJECT_INGEST (F10) · ANALYSIS_ACCURACY (F10, minor) · PERFORM_SURFACE (F7) · GIG_RESILIENCE (F8/F17/F19) · AUDIO_OBJECT_TRACKING (F11 header) · APP_SHELL + AUDIO_SENDS_UX + AUDIO_INFRASTRUCTURE (F12) · SCENE_BUILD + COMPONENT_LIBRARY (F14) · GAUSSIAN_SPLATS (F4) · UNBOUNDED_LIGHTS (F3 prereq line) · MULTI_DISPLAY + MEDIA_BACKEND + ML_NODES (F15 pointers) · UI_AUTOMATION (F18) · ABLETON_SHOW_SYNC (F9 note).

**Clean (composition-coherent as written):** SESSION_MODE · MULTI_DISPLAY (body; F15 pointer only) · APP_SHELL (body; F12 note only) · SIMULATIONS · BOX3D_PHYSICS · SCENE_BUILD (body; F14 note only) · KICK_SWEEP_EVENT · AUDIO_SETUP_DOCK · MAPPING_GRAMMAR · AUTO_POPULATE · VIDEO_IO · MEDIA_BACKEND (body) · COMMERCIALIZATION · LIVE_RECORDING_PROOFS · UI_HARNESS_UNIFICATION · PERF_BUDGET_GATE · UI_AUTOMATION (body) · VULKAN_BACKEND · LED_STRIPS · PROJECTION_MAPPING · DJ_PERFORMANCE · PRO_DJ_LINK · MCP_INTERFACE · COMPONENT_LIBRARY (body) · ML_NODES (body) · PARAM_STORAGE (clean-with-remainder: Peter's library re-save).

**Stale anchors (re-derive at brief time, no amendment owed):** OBJECT_TRACKING P5 (F13) · PRO_DJ_LINK sync-seam anchors (ABLETON_TRANSPORT_SYNC landed) · SCENE_BUILD header's "BOUNDARIES must land before P3" phrasing (prereq now satisfied) · every doc auditing render_scene.rs line numbers once any F3 sibling lands.

## Handoff split

**Opus amendment session — DONE 2026-07-10 (this landing).** Peter's decisions applied:
shadow caster cap `K = MAX_SHADOW_CASTING_LIGHTS = 4` (first-K-lights policy); F8 rejoin =
build it.
1. ✅ F2 — REALTIME_3D: D4 sets the `MAX_SHADOW_CASTING_LIGHTS = 4` caster cap (first-K by
   slot, extras still illuminate); D3/§3/§6/§7.3 stale "8 objects, 4 lights" cap text struck
   and re-anchored to `OBJECT_SLIDER_MAX = 64` + UNBOUNDED_LIGHTS; P2 gate gains the
   >K-casters assertion.
2. ✅ F5/F6 — IMPORT: reality note refreshed (the assembler `assemble_import_graph` +
   per-material groups + camera/sun/IBL + `ImportReport` + single-undo-transaction install
   already ship; placement resolved = manifold-renderer, D1/§3's manifold-io superseded); P1
   re-cut to remaining work (light mapping, camera consumption, report surfacing, alphaMode/
   normal/doubleSided report lines, hierarchy pre-compose, cap 8→64, Khronos conformance).
3. ✅ F10 — OBJECT_INGEST D5 (consume ANALYSIS_ACCURACY's `eval/`, don't fork) + D4 sequenced
   after ANALYSIS P3/P4 baselines + chord-emitter seam named; ANALYSIS_ACCURACY companion +
   §4.3 seam note added.
4. ✅ F4 — GAUSSIAN_SPLATS D10 re-anchored to render_mesh/render_copies; Transform-port
   override deferred to SCENE_BUILD §9's shared trigger (F4b judgment); SCENE_BUILD §9 names
   render_splats (F4a/c folded in).
5. ✅ F14 — SCENE_BUILD (audit row + Rejected + Decided-3 + §9) and COMPONENT_LIBRARY §4 both
   amended: the kill target is a *live group-param runtime*, not the `GroupParamDef` type;
   COMPONENT_LIBRARY reuses it declaration-only (lowers to card `BindingDef`s); sections ⊥ macros.
6. ✅ F8 — GIG_RESILIENCE D8 caveat truth-fixed (inbound `song_time` listener landed in
   ABLETON_TRANSPORT_SYNC, `:242/:891`); `--resume` Ableton-position rejoin added as named P4
   remaining work per Peter's decision to build it.

**Still owed (mechanical, any Sonnet session; no judgment):** F3 · F7 · F9 · F11 · F12 · F13 · F15 · F17 · F18 · F19 + board regen after the header fixes. *(F4a/c and F6 folded into the Opus edits above.)*

**Fixed earlier this session:** F1/F16 (DESIGN_BUILD_ORDER update, committed with this ledger).
