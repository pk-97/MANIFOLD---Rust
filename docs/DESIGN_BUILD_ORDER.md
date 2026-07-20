# Design Build Order — dependency-sequenced roadmap for the approved designs

**Status: NORMATIVE, living · 2026-07-03 · Fable (part of the doc hardening pass) · corpus-refreshed 2026-07-10 by the cross-design coherence audit (`docs/archive/DESIGN_COHERENCE_AUDIT_2026-07-10.md`): 10 missing designs added, 6 shipped rows removed, 2 stale ⏸ annotations fixed, render_scene same-file sequencing note added**
**Scope: every APPROVED-not-built design doc. Update when a design ships, a new one
lands, or Peter re-ranks.**

Two kinds of ordering, kept separate on purpose:

- **Hard edges** — design B literally cannot be built before A. Fixed; violating one
  produces rework by construction.
- **Recommended order** — value and cost-of-delay judgment. Peter re-ranks these
  freely; the hard edges are the only fence.

An executing agent starting any design: check this doc first — if the design's hard
prerequisites aren't shipped, stop.

---

## 1. The corpus (approved, not built)

> **Status source = the DESIGN STATUS BOARD** (`python3 .claude/hooks/design_status.py`,
> auto-injected each session from the design docs' status lines). The status notes in
> this table are navigational and can lag — trust the board for state; trust this table
> for the dependency/ordering columns, which is its real job.

| Design doc | Hard prerequisites | Hardening level (§DESIGN_DOC_STANDARD §9) |
|---|---|---|
| GIG_RESILIENCE_DESIGN (P1–P2 SHIPPED 2026-07-03; P3–P4 remain) | P3 after PERFORM_SURFACE P1 (see §3) | full |
| MULTI_DISPLAY_DESIGN (P1 ✅; §6.1a resolved 2026-07-06 — **P2 RE-ISSUABLE**; P3–P5 remain) | none | full |
| SESSION_MODE_DESIGN (P1–P3 ✅; P4 grid UI = Peter feel-review; P5 recording) | none | full |
| MEDIA_BACKEND_DESIGN (§3a resolved 2026-07-06 — **P1 RE-ISSUABLE**) | none for P1–P3; §6 pairs with VULKAN §8 | full |
| PERFORM_SURFACE_DESIGN | none for P1; P2 needs SESSION_MODE **through P4's ContentState plumbing** (the session snapshot fields are greenfield P4 work per its 2026-07-05 baseline review) | full |
| LED_STRIPS_DESIGN | none for P1; P2 needs MULTI_DISPLAY P1–P3 | conformance |
| PROJECTION_MAPPING_DESIGN | MULTI_DISPLAY P1–P3 | conformance |
| ABLETON_SHOW_SYNC_DESIGN | none. Brief-time note (audit F9): its D16 lanes deferral trigger has FIRED (AUTOMATION_LANES shipped) — decide fold-in vs keep-deferred at brief | conformance |
| COMPONENT_LIBRARY_DESIGN | VOCAB apply ✅; C3 after node-groups canvas | conformance |
| MCP_INTERFACE_DESIGN | VOCAB apply ✅; wants COMPONENT_LIBRARY (its authoring surface) | conformance |
| ML_NODES_DESIGN | none for Vision/CoreML tier; ONNX tier needs VULKAN | conformance |
| VULKAN_BACKEND_DESIGN | none (Phase 0 scaffold shipped 0c5dde17) | conformance |
| REALTIME_3D_DESIGN (P0/P1/P2/P3/P4/§9 ✅; P5–P7 remain) | **P2 shadows + P3 fog SHIPPED 2026-07-11** (`bf0e1a5d`; gpu-proofs `render_scene_shadows`+`render_scene_fog`, PNG-verified; F2 caster policy built as specified); P6 needs SCENE_BUILD P2 (amended D3/D8) | full |
| SIMULATIONS_DESIGN | REALTIME_3D P1 ✅ | full |
| IMPORT_FIDELITY_DESIGN (added 2026-07-15) | none — all prereqs in-tree (MATERIAL M1–M6, REALTIME_3D P1–P3/P8/P9, shipped glTF assembler); **outranks IMPORT_DESIGN P1-remaining** (Peter: "really critical infra"); PROPOSED — Peter's read pending | full |
| IMPORT_DESIGN | **P1: scope re-cut first per coherence audit F5** (reality note understates the shipped `build_import_graph` scene importer); **P1-remaining now orders AFTER IMPORT_FIDELITY (2026-07-15), whose F-P4 absorbs §8's normal-map report scope**; P1–P3 prereqs (REALTIME_3D P1 + MATERIAL M6) ✅; P5 needs SESSION_MODE + MEDIA_BACKEND P2; P6 agent half needs MCP | full |
| COMMERCIALIZATION_DESIGN | none hard; P4 telemetry rides GIG_RESILIENCE P1–P2 ✅; AUDIO_ANALYSIS_ACCURACY P2+P6 (BUG-069) before launch | conformance |
| DJ_PERFORMANCE_DESIGN | ABLETON_SHOW_SYNC; PERFORM_SURFACE P1; MEDIA_BACKEND P1 | conformance |
| PRO_DJ_LINK_DESIGN | PERFORM_SURFACE P1; sync-source seam (re-derive anchors — ABLETON_TRANSPORT_SYNC landed 2026-07-07) | conformance |
| UI_AUTOMATION_DESIGN (P1–P2 ✅; P3–P4 remain) | none; UI_HARNESS P2 rewrites the shipped Runner — re-derive anchors (audit F18) | full (P1–P2) / conformance (P3–P4) |
| TIMELINE_INGEST_DESIGN (P3–P5 ✅) | P1/P2 PARKED on BUG-028 | full |
| GAUSSIAN_SPLATS_DESIGN | none hard (P4 consumes shipped `render_scene`); D10 re-anchor owed before P3 (audit F4) | full |
| SCENE_BUILD_AND_GROUP_PARAMS_DESIGN ✅ **WAVE COMPLETE 2026-07-10** (P1–P5 all shipped: Transform port+atom, render_scene port swap+v1.12.0 migration+importer, card sections, group-face rows, add-object/light+ribbons) — Peter L4 feel-pass on P5 gestures owed | (satisfied) REALTIME_3D P6 now unblocked (its P2 dep landed) | full |
| APP_SHELL_DESIGN | none for P1–P3; its §8 slots bind their own waves | full (P1–P3) / conformance (§8 slots) |
| VIDEO_IO_DESIGN (added 2026-07-09) | none for P1–P2; P3–P4 need the NDI SDK verify (D8; Peter if ambiguous) | full |
| UI_HARNESS_UNIFICATION_DESIGN (added 2026-07-09) | none unbuilt (UI_AUTOMATION P1–P2 + UI_CLIP_AND_Z P1 shipped) | full |
| PERF_BUDGET_GATE_DESIGN (added 2026-07-09) | none (UI_HARNESS P0 improves the numbers, doesn't block) | conformance |
| LIVE_RECORDING_PROOFS_DESIGN (added 2026-07-09; **release-gating**, owns BUG-053) | none | full |
| BOX3D_PHYSICS_DESIGN (added 2026-07-09) | none for P1–P3; P4 wants depth-estimate (shipped) | full |
| AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN (added 2026-07-09) | none | full |
| AUDIO_ANALYSIS_ACCURACY_DESIGN (added 2026-07-08; = §3 item 13g) | none; P2+P6 gate COMMERCIALIZATION (BUG-069) | full |
| MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN (added 2026-07-10) | none hard (`render_scene` + SCENE_BUILD P1–P3 ✅, `triangulate_grid` shipped); release-content authoring (scanned-flowers pieces); glTF *animation* explicitly deferred to its own future design | full |
| RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN (✅ BUILT + LANDED 2026-07-10; single phase) | none — see the §2 render_scene same-file note; anchors now MOVED, re-derive before SCENE_BUILD P2 / GAUSSIAN P4 | full |
| AUDIO_OBJECT_TRACKING_DESIGN (P1–P4 ✅ 2026-07-06; **P5 scope overlay + BUG-045 remain**) | none; P5 re-derives scope anchors (ScopeColumn typed-overlay refactor landed 2026-07-07) | full |
| AUDIO_OBJECT_INGEST_DESIGN | OBJECT_TRACKING P0–P2 ✅; P1 blocked on Peter (labeled clips) **and on the F10 reconciliation with ANALYSIS_ACCURACY's eval harness** | conformance |
| KICK_SWEEP_EVENT_DESIGN (P1/P2/P4/P5 ✅) | P3 feel-pass = Peter-owned (L4, not agent-executable) | full |
| CAMERA_AND_LENS_DESIGN (added 2026-07-12) | none (REALTIME_3D P1–P3 ✅); its P1 oracle gates GBUFFER P1's conformance test | full |
| GBUFFER_DESIGN (added 2026-07-12; the RENDERING_INFRA_V2 §2 keystone) | CAMERA_AND_LENS P1 (oracle) for its P1 gate; PERF_BUDGET_GATE measures its bandwidth line but does NOT block (lazy rule is the cost control until then) | full |
| CINEMATIC_POST_DESIGN (added 2026-07-12) | P1/P2 need CAMERA_AND_LENS P1–P2 + GBUFFER P1; P3 needs GBUFFER P2 | full |
| REALTIME_3D §11 P9 PCSS (added 2026-07-12) | none (P2 shadows ✅); independent of the three docs above | full |

Directions, pre-queue (not designed to STANDARD, no rows): MAPPING_GRAMMAR (first
card = future Peter discussion) · AUTO_POPULATE (Opus design owed; gated on the
BUG-069 rework).

Rows removed as SHIPPED per §5 (2026-07-10 coherence audit): AUTOMATION_LANES ·
MATERIAL_SYSTEM · OVERLAY_SESSIONS_AND_PICKER · PRESET_LIBRARY · PARAM_STORAGE
(P1–P5; Peter's one-time library re-save remains, not a build item) ·
PARAM_STORAGE_BOUNDARIES.
Not in the queue: **LIVE_AUDIO_TRIGGERS** is SHIPPED (phases 0–7, proven live,
branch merged). **COMPETITIVE_STEAL_PASS** is a closed record.

## 2. Hard dependency edges

```
VOCAB apply ──────────→ COMPONENT_LIBRARY ──→ MCP_INTERFACE
MULTI_DISPLAY P1–P3 ──→ PROJECTION_MAPPING
MULTI_DISPLAY P1–P3 ──→ LED_STRIPS P2   (LED P1 is free)
SESSION_MODE ─────────→ PERFORM_SURFACE P2  (P1 is free)
PERFORM_SURFACE P1 ───→ GIG_RESILIENCE P3   (arming targets the chrome-hosted mode)
VULKAN ───────────────→ ML_NODES ONNX tier · MEDIA_BACKEND §6 (Vulkan-era handoff)
MATERIAL_SYSTEM ✅ ────→ REALTIME_3D  (its P0 — satisfied 2026-07-04)
MATERIAL M6 ──────────→ IMPORT P1  (albedo/metallic maps + alpha cutout; MATERIAL §11)
REALTIME_3D P1 ───────→ SIMULATIONS  (sims render into render_scene; cloth can smoke-test earlier)
REALTIME_3D P1 ───────→ IMPORT P1–P3 · SESSION_MODE + MEDIA_BACKEND P2 → IMPORT P5 (Resolume) · MCP → IMPORT P6 agent half
OVERLAY_SESSIONS P2 ──→ PRESET_LIBRARY P5  (the browser rides PickerCore; P1–P4 are free)
```

Everything else is edge-free and orders by judgment only.

**Same-file co-claimants — `render_scene.rs` (coherence audit F3, 2026-07-10):** three
unbuilt designs edit the same primitive's `rebuild`/`evaluate` (and one its shader):
SCENE_BUILD_AND_GROUP_PARAMS P2 (param→Transform-port swap), RENDER_SCENE_UNBOUNDED_LIGHTS
P1 (uniform + WGSL + light ports), GAUSSIAN_SPLATS P4 (lazy `depth` output). Any pairwise
order works, but they must be **sequenced, never concurrent**, and whichever runs later
re-derives every render_scene anchor (each doc's audit line numbers break when a sibling
lands). Recommended order: UNBOUNDED_LIGHTS first (smallest, one phase), then SCENE_BUILD
P2, then SPLATS P4.

**Why VOCAB is first overall:** ~75 `type_id` renames touch presets, bindings,
saved projects, and docs. Every preset, component, binding, and MCP surface built
before the rename enlarges the apply pass and its migration table. Cost of delay is
strictly increasing; nothing depends on delaying it. It also unblocks the whole
authoring/AI track.

**Why GIG_RESILIENCE P3 waits for PERFORM_SURFACE P1:** verified against both docs
2026-07-03. The arming *state hooks* (spawn understudy, park autosave, reset crash
counter) attach to perform mode's enter/exit lifecycle
(`perform_mode/mod.rs` `pending_enter`/`pending_exit`), which PERFORM_SURFACE P1
preserves — that part is order-independent, no architectural conflict. But P3's
*visible* pieces — the Cmd+Q hold-progress indicator and the understudy status
strip — are perform-HUD rendering: built before P1 they'd be hand-drawn and
rewritten as widgets one phase later. Order them; don't build twice.

## 3. Recommended order (re-rankable)

**Version map (Peter, 2026-07-03 — see BUSINESS_PLAN.md §2):**
**v1.0 = waves 0–3 complete + the launch gate below** — core feature-complete,
"just works out of the box." **v1.x** = small flexes (staged ML Vision tasks,
preset/component packs) + the **import funnels as campaigns** — IMPORT P5
(Resolume) and P6 (TD) have all hard prerequisites inside v1.0, so they fill the
otherwise-quiet Vulkan stretch. **v1.5 = Vulkan/Windows** (+ ML ONNX parity, its
hard edge). **v2.0 = the 3D track (§4) as the paid upgrade.**

Within a wave, re-rank freely by judgment — including **demo-ability** (does the
feature make a release trailer?): deeply-useful-but-boring-to-film items ride
along with a flashy headliner rather than headlining.

Grouped in waves; within a wave, items are independent and order is free.

**Wave 0 — lock the names, protect the work. ✅ COMPLETE (2026-07-03, merged into `feat/timeline-ui-redesign`).**
1. ✅ VOCAB apply pass — P1–P7 shipped; every "VOCAB apply" prerequisite below (COMPONENT_LIBRARY, MCP_INTERFACE, MATERIAL_SYSTEM, REALTIME_3D, IMPORT P6) is now satisfied.
2. ✅ GIG_RESILIENCE P1 (autosave + save-error surfacing + crash.log rotation). Two manual checks still owed by Peter (autosave end-to-end; read-only-volume dialog).

**Wave 1 — the stage foundations.** (2026-07-03: P1s landing in parallel worktrees off `feat/timeline-ui-redesign`.)
4. MULTI_DISPLAY P1–P3 (core model → island rendering → multi-output present) — the
   widest unblock in the corpus: projection mapping, LED P2, and the rig work all
   ride it. **P1 ✅ merged (`0cb5114f`); §6.1a seam committed 2026-07-06 — P2 RE-ISSUABLE** (the hardening-queue item is resolved).
5. SESSION_MODE — second performance surface; unblocks session perform. **P1 (`4f072100`) + P2 (`f852d2bc`) + P3 (`9a069aa4`) ✅ merged. P4 = the grid UI — hand to Peter for feel review, not auto-gated. P5 = recording.**
6. MEDIA_BACKEND P1–P3 (Metal era) — decode/encode traits; independent of everything.
   **§3a addendum resolved 2026-07-06 (FrameLease + MediaBackends committed against the
   shipped async protocol) — P1 RE-ISSUABLE.**
7. GIG_RESILIENCE P2 (breadcrumb + `--resume`) — needs nothing from other designs. **✅ merged (`3dffe29a`). D8 Ableton-position rejoin deferred to ABLETON_SHOW_SYNC (bridge has no inbound song-position); breadcrumb-beat fallback shipped. P3 (understudy) needs PERFORM_SURFACE P1.**

**Wave 2 — the show becomes playable end-to-end.**
8. PERFORM_SURFACE P1 (chrome-hosted perform substrate, timeline perform migrated).
9. GIG_RESILIENCE P3 (understudy + arming) — immediately after 8, per the hard edge.
10. PERFORM_SURFACE P2 (session perform) — after 5.
11. PROJECTION_MAPPING (corner-pin first — serves the GT2000HDR portrait rig).
12. LED_STRIPS P1 (patch + sACN, no island dep), then P2–P3 after 4.
13. ABLETON_SHOW_SYNC — the score importer; makes gig-resilience rejoin exact and is
    the core Ableton workflow. No hard edge, high instrument value.

**Wave 3 — authoring depth + the AI surface.**
13b. OVERLAY_SESSIONS_AND_PICKER P1–P2 → PRESET_LIBRARY P1–P6 (added 2026-07-04,
    Peter-driven: preset/graph management rethink). Authoring-UX pair serving the
    release-content push; OVERLAY P1 also fixes a live stale-search bug, so it is
    re-rankable arbitrarily early — nothing depends on it and it depends on nothing.
    **PRESET_LIBRARY P0 (added 2026-07-04 evening) fixes the live glTF-import
    black-box bugs (BUG-016) and depends on nothing — run it FIRST, ahead of any
    wave, next Sonnet session.**
13c. TIMELINE_INTERACTION_P1_SPEC P1.0–P1.6 (added 2026-07-04, Peter-driven: five
    in-app interaction bugs, one authority-duplication disease). Sequel to the
    shipped TIMELINE_LAYOUT_P0; serves the release-content push directly (trim/
    duplicate/multi-select ARE the composing loop), so like 13b it is re-rankable
    arbitrarily early. Hard edge: runs BEFORE UI_CRAFT_AND_MOTION_PLAN (same
    files; motion must not animate lying previews).
13d. TIMELINE_INGEST_DESIGN P1–P5 (added 2026-07-04, Peter-driven: file drops aim
    at a stale cursor so audio spawns new lanes instead of joining the lane under
    the pointer; Finder-clipboard paste; replace-audio-in-place; role-keyed stem
    lanes). The drop→detect→compose loop is the authoring front door, so it serves
    the release-content push directly; zero hard edges, re-rankable arbitrarily
    early like 13b/13c. P1 opens with a VERIFY-AT-IMPL prototype gate (§3 of the
    doc) — run that check before committing to the wave.
13d2. PARAM_STORAGE_DESIGN P1–P5 (SHIPPED 2026-07-05, closed 2026-07-16 —
    remaining items are Peter-owned: V1.4 library re-save + VD-007–VD-010).
    Id-keyed
    per-instance param manifest; registry demoted to a load/instantiation
    template; one-time migration kills the positional wire arms. Zero hard
    edges, but Peter ranked it AHEAD of new authoring features (2026-07-05):
    every card feature built before it inherits the positional-identity bug
    class (driver misroutes, Ableton/OSC blind spots on imported generators),
    so cost of delay is strictly increasing — run it before 13b–13e's
    remaining phases where scheduling allows. P2 (the storage swap) is the
    heavy session; strong-model executor recommended.
13e. AUDIO_SENDS_UX_DESIGN P1–P5 (added 2026-07-04, Peter-driven: "sends work but
    they're awkward to use and tricky to understand"). Send-path view, per-send
    analysis gating (perf: one bound param currently analyzes all 16 sends,
    ~1 ms/tick on the content thread), non-dim right-anchored panel, Send→Source
    string rename, drawer presets. Zero hard edges, re-rankable arbitrarily early
    like 13b/13c; P1 (gating + doc truth pass) is a small standalone win.
13f. APP_SHELL_DESIGN P1–P3 (added 2026-07-06, Peter-driven design session: menus,
    settings taxonomy, shell furniture). Command table + full menu (P1), Project
    Settings window (P2), Settings window + typed AppPrefs (P3). Zero hard edges;
    re-rankable arbitrarily early. Two soft synergies: its §8 slots are what
    MULTI_DISPLAY/PROJECTION/LED/COMMERCIALIZATION config surfaces land into, so
    shipping it before those waves start saves them idiom-inventing; and P1's
    command table is what MCP_INTERFACE's command surface reads.
13g. AUDIO_ANALYSIS_ACCURACY_DESIGN P1–P7 (added 2026-07-08, Peter-driven: offline
    detection accuracy + eval harness over public datasets + Beat This swap +
    sustained-object/section clips). Zero hard edges — all tools/audio_analysis
    Python except P5's small Rust seams; re-rankable arbitrarily early like
    13b–13e. Two forcing functions: P2+P6 remove the NC-licensed madmom models
    (BUG-069, a commercialization blocker — must land before item 19), and the
    detection-accuracy phases (P4–P5) serve the release-content push (import
    front door). P7 (unattended tuning loop) is post-release; Peter re-ranks.
13h. **2026-07-09 approvals (added by the 2026-07-10 audit; all zero-hard-edge —
    RANKED by Peter 2026-07-10):**
    1. UI_HARNESS_UNIFICATION P0–P3 (first — build starting 2026-07-10; P0 = the
       BUG-060 red bracket; dev infra whose value compounds like UI_AUTOMATION's).
    2. LIVE_RECORDING_PROOFS P1–P3 (**release-gating** — must land inside v1.0;
       no dependency on the harness — different crates — so it may run in
       parallel with it in a separate worktree).
    3. AUDIO_SETUP_DOCK P1–P4 (calibration loop + trigger unification; serves
       set-prep directly). ✅ **WAVE COMPLETE 2026-07-10** — P1–P4 all shipped (`36a96791`, `e4aa01bf`, `47f2a112`, `5c4fbcca`, `12fbc37d`, `a649f62a`); closed BUG-047/070/082. Peter L4 feel-pass + Cap+N/St/Mo tooltips (Deferred) owed.
    4. BOX3D_PHYSICS P1–P4 (differentiator, not release-gating; Peter: opens new
       visuals).
    Parked at the back (Peter 2026-07-10): VIDEO_IO P1–P4 (join-existing-rigs
    interop; highly demoable, but waits) · PERF_BUDGET_GATE P1–P2 (small standing
    frame-budget fence; Peter: "more of an optimisation task").
14. COMPONENT_LIBRARY → 15. MCP_INTERFACE (in that order — MCP consumes components).
16. AUTOMATION_LANES.
17. ML_NODES Vision/CoreML tier.
18. GIG_RESILIENCE P4 (peripheral hardening — MIDI hotplug, audio rebuild, GPU CB
    status, thermal).

**Launch gate (inside v1.0, after wave 3):**
19. COMMERCIALIZATION_DESIGN P1–P4 (license file, trial watermark, updater,
    telemetry upload).
20. First-hour pass: bundled demo project + starter presets; `.als` import is the
    first hour (BUSINESS_PLAN §7) — re-weighting, not new design.

**Unranked post-v1.0 candidates (Peter ranks; prereqs are all inside v1.0):**
- DJ_PERFORMANCE (library/crate-compile/two-deck orchestration over Ableton) and
  PRO_DJ_LINK (native CDJ sync + track-triggered scores). Both are v1.x-eligible
  and highly demoable — strong campaign material alongside the import funnels.

**Background track (long-running, parallel to everything):**
- VULKAN phases 1–4 = **the v1.5 release**. Start gates on v1.0 stability in the
  field; nothing in waves 0–3 waits on it, and its two dependents (ML ONNX,
  MEDIA_BACKEND §6) land with it.

**Dev-infra track (not a product feature; slot by judgment, recommended early):**
- UI_AUTOMATION P1–P2 (selector dump + headless script driver) — zero hard
  edges, extends the shipped ui-snap harness. Its value compounds: every UI
  phase built after it (perform surface, session grid, projection UX) gets
  scripted integration flows instead of hand-verification, so earlier is
  strictly better. P3–P4 (live door) whenever the live loop is wanted.

## 4. The 3D track (added 2026-07-03 · re-ranked 2026-07-04)

MATERIAL M1–M5 shipped (verified 2026-07-04). **Peter greenlit the import wave
2026-07-04** (the stewartia conversation — get CC0/Blender models rendering with
full materials, relightable, in HDRI-lit multi-object scenes with movable cameras
and mesh-explode particles): the track's first arc starts now, parallel to the
release-content push, rather than waiting for v2.0. The version map's "3D track =
paid v2.0 upgrade" still holds for the REMAINDER (simulations, viewport tiers,
full polish) — this wave builds the track's foundation early because it is also
release-content authoring capability.

**The greenlit wave, in dependency order (~5–6 sessions):**

0. **Wave-0 verification (first session, before any new code):** runtime-verify
   MATERIAL M1–M5 — load MetallicGlass, headless-render to PNG, look at it. The
   tranche checkmarks are static; nobody has confirmed a shipped PBR frame.
   Then MATERIAL §11.3 entry-state checks.
1. **MATERIAL M6** (§11 — albedo/metallic maps, alpha cutout, back-face fix).
   One session. No dependents inside REALTIME_3D P1, so it may also run parallel
   to (2).
2. **REALTIME_3D P1 — `render_scene` + multi-light.** 1–2 sessions (workspace-sweep
   gated).
3. **IMPORT P1 — glTF door.** 1–2 sessions. Needs (1) + (2). **Peter-hands
   prerequisite:** download the stewartia .glb into `tests/fixtures/gltf/`
   (Sketchfab needs a login — IMPORT §8 addendum).
4. **REALTIME_3D P4 — camera atoms** (free_camera / look_at_camera). One session;
   only needs P1.
5. **IMPORT P4 — texture-set auto-wire + HDRI drop.** One session; needs M6 only.
6. **`node.spawn_from_mesh`** (REALTIME_3D §9 addendum). Small; no prerequisites
   at all — a filler task for any session with slack.

Then, still greenlit but second arc (order by judgment): REALTIME_3D P2 shadows →
P3 atmosphere → P5 viewport navigate → P6 gizmos → P7 starter preset.
**SCENE_BUILD_AND_GROUP_PARAMS (added 2026-07-06) slots INSIDE this arc, ahead of
P6:** its P1–P2 (Transform port + `node.transform_3d`, render_scene swap + migration
+ importer) are a hard prerequisite of P6 gizmos (REALTIME_3D D3/D8 amended — gizmos
write the transform atom's params). Its P3 (card sections) additionally needs
PARAM_STORAGE_BOUNDARIES P1–P2 first; P4/P5 hang off its own P2/P3 only. It is also
release-content authoring (scene-building UX for the August push), so by judgment it
runs EARLY in the arc.
**GAUSSIAN_SPLATS_DESIGN (added 2026-07-05, Peter-flagged "will definitely be
important") slots into this arc**: zero hard prerequisites (its P4 consumes the
shipped `render_scene`), so its P1–P3 are startable any session; it is also
release-content authoring (photoreal scan material), so it may be re-ranked ahead
of the second-arc REALTIME_3D phases by judgment. Early
placement before P5/P6 is inspector sliders + agent-edited preset JSON verified
by headless PNG — workable, per Peter.
**MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN (added 2026-07-10) slots into this arc the
same way as splats**: zero hard prerequisites, release-content authoring (Peter's
scanned-flowers pieces — grow/unfold/morph/dissolve), Sonnet-sized phases P1–P4;
re-rankable ahead of the second-arc REALTIME_3D phases by the same judgment clause.

IMPORT P5 (Resolume) and P6 (TD) still hang off the main waves (session mode /
MCP) as v1.x campaigns. SIMULATIONS still waits (v2.0).

**Orchestration notes (for the layer running this):** read
`project_agent_execution_playbook` hazards before starting · every phase brief's
read-back + entry-state checks are mandatory, including re-running §11.1/§8
anchors · phases gate on rendered PNGs where stated, not green tests alone ·
branch per phase off the integration branch, commit by path (never `add -A`).

## 5. Maintenance

When a design ships: flip its doc's status line, remove its row here, and re-check
edges that pointed at it. When Peter re-ranks: move rows between waves freely; hard
edges (§2) are the only invariant. This doc is the single place build order lives —
individual design docs state their own prerequisites but never sequence each other.
