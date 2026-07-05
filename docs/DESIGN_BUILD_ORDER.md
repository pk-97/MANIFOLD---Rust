# Design Build Order — dependency-sequenced roadmap for the approved designs

**Status: NORMATIVE, living · 2026-07-03 · Fable (part of the doc hardening pass)**
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

| Design doc | Hard prerequisites | Hardening level (§DESIGN_DOC_STANDARD §9) |
|---|---|---|
| GIG_RESILIENCE_DESIGN (P1–P2 SHIPPED 2026-07-03; P3–P4 remain) | P3 after PERFORM_SURFACE P1 (see §3) | full |
| MULTI_DISPLAY_DESIGN | none | full |
| SESSION_MODE_DESIGN | none | full |
| MEDIA_BACKEND_DESIGN | none for P1–P3; §6 pairs with VULKAN §8 | full |
| PERFORM_SURFACE_DESIGN | none for P1; P2 needs SESSION_MODE built | full |
| LED_STRIPS_DESIGN | none for P1; P2 needs MULTI_DISPLAY P1–P3 | conformance |
| PROJECTION_MAPPING_DESIGN | MULTI_DISPLAY P1–P3 | conformance |
| ABLETON_SHOW_SYNC_DESIGN | none | conformance |
| AUTOMATION_LANES_DESIGN | none | conformance |
| COMPONENT_LIBRARY_DESIGN | VOCAB apply (names are the vocabulary) | conformance |
| MCP_INTERFACE_DESIGN | VOCAB apply; wants COMPONENT_LIBRARY (its authoring surface) | conformance |
| ML_NODES_DESIGN | none for Vision/CoreML tier; ONNX tier needs VULKAN | conformance |
| VULKAN_BACKEND_DESIGN | none (Phase 0 scaffold shipped 0c5dde17) | conformance |
| MATERIAL_SYSTEM_DESIGN (**M1–M5 SHIPPED**, verified 2026-07-04; M6 addendum §11 remains) | none (VOCAB ✅) | conformance + §11 addendum |
| REALTIME_3D_DESIGN (P0 ✅ = MATERIAL M1–M5) | none remaining (VOCAB ✅) | full (written to standard 2026-07-03) |
| SIMULATIONS_DESIGN | REALTIME_3D P1 for scene composition | full (written to standard 2026-07-03) |
| IMPORT_DESIGN | per phase: P1–P3 need REALTIME_3D P1 + **MATERIAL M6**; P5 needs SESSION_MODE + MEDIA_BACKEND P2; P6 needs VOCAB ✅ (agent half: MCP) | full (written to standard 2026-07-03; §8 addendum 2026-07-04) |
| COMMERCIALIZATION_DESIGN (commerce infra: license, watermark, updater, telemetry) | none hard; P4 telemetry rides GIG_RESILIENCE P1–P2 | conformance |
| DJ_PERFORMANCE_DESIGN | ABLETON_SHOW_SYNC; PERFORM_SURFACE P1; MEDIA_BACKEND P1 | conformance |
| PRO_DJ_LINK_DESIGN | PERFORM_SURFACE P1; sync-source seam (timecode/Link infra) | conformance |
| UI_AUTOMATION_DESIGN | none (dev infra; P1–P2 full, P3–P4 conformance) | full (P1–P2) / conformance (P3–P4) |
| OVERLAY_SESSIONS_AND_PICKER_DESIGN (added 2026-07-04) | none (extends shipped overlay driver) | full |
| PRESET_LIBRARY_DESIGN (added 2026-07-04) | P5 needs OVERLAY_SESSIONS P2; P6 verify-at-impl gated | full (P1–P4) / conformance (P5–P6) |
| TIMELINE_INGEST_DESIGN (added 2026-07-04) | none | full |
| GAUSSIAN_SPLATS_DESIGN (added 2026-07-05) | none (its P4 consumes shipped `render_scene`) | full |
| PARAM_STORAGE_DESIGN (added 2026-07-05) | none | full |

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
   ride it. **P1 ✅ merged (`0cb5114f`); P2 ⏸ BLOCKED pending §6.1 seam-hardening — `docs/DESIGN_HARDENING_QUEUE.md` item 2.**
5. SESSION_MODE — second performance surface; unblocks session perform. **P1 (`4f072100`) + P2 (`f852d2bc`) + P3 (`9a069aa4`) ✅ merged. P4 = the grid UI — hand to Peter for feel review, not auto-gated. P5 = recording.**
6. MEDIA_BACKEND P1–P3 (Metal era) — decode/encode traits; independent of everything.
   **P1 ⏸ PARKED 2026-07-03: committed §3 trait can't wrap the shipped thread-split +
   zero-alloc reuse-pool decode — needs a §3 addendum before code
   (`docs/DESIGN_HARDENING_QUEUE.md` item 1). No near-wave dependent, so parking is free.**
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
13d2. PARAM_STORAGE_DESIGN P1–P5 (added 2026-07-05, Peter-driven). Id-keyed
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
**GAUSSIAN_SPLATS_DESIGN (added 2026-07-05, Peter-flagged "will definitely be
important") slots into this arc**: zero hard prerequisites (its P4 consumes the
shipped `render_scene`), so its P1–P3 are startable any session; it is also
release-content authoring (photoreal scan material), so it may be re-ranked ahead
of the second-arc REALTIME_3D phases by judgment. Early
placement before P5/P6 is inspector sliders + agent-edited preset JSON verified
by headless PNG — workable, per Peter.

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
