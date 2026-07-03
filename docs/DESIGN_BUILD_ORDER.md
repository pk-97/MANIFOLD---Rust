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
| GIG_RESILIENCE_DESIGN (P1 SHIPPED 2026-07-03; P2–P4 remain) | none for P2; P3 after PERFORM_SURFACE P1 (see §3) | full |
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
| MATERIAL_SYSTEM_DESIGN | VOCAB apply (renames its renderer ids) | conformance |
| REALTIME_3D_DESIGN | MATERIAL_SYSTEM M1–M5 (its P0); VOCAB apply | full (written to standard 2026-07-03) |
| SIMULATIONS_DESIGN | MATERIAL M1–M5; REALTIME_3D P1 for scene composition | full (written to standard 2026-07-03) |
| IMPORT_DESIGN | per phase: P1–P3 need REALTIME_3D P1 + MATERIAL; P5 needs SESSION_MODE + MEDIA_BACKEND P2; P6 needs VOCAB (agent half: MCP) | full (written to standard 2026-07-03) |
| COMMERCIALIZATION_DESIGN (commerce infra: license, watermark, updater, telemetry) | none hard; P4 telemetry rides GIG_RESILIENCE P1–P2 | conformance |
| DJ_PERFORMANCE_DESIGN | ABLETON_SHOW_SYNC; PERFORM_SURFACE P1; MEDIA_BACKEND P1 | conformance |
| PRO_DJ_LINK_DESIGN | PERFORM_SURFACE P1; sync-source seam (timecode/Link infra) | conformance |
| UI_AUTOMATION_DESIGN | none (dev infra; P1–P2 full, P3–P4 conformance) | full (P1–P2) / conformance (P3–P4) |

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
MATERIAL_SYSTEM ──────→ REALTIME_3D  (its P0; glTF import later rides REALTIME_3D)
REALTIME_3D P1 ───────→ SIMULATIONS  (sims render into render_scene; cloth can smoke-test earlier)
REALTIME_3D P1 ───────→ IMPORT P1–P3 · SESSION_MODE + MEDIA_BACKEND P2 → IMPORT P5 (Resolume) · MCP → IMPORT P6 agent half
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
5. SESSION_MODE — second performance surface; unblocks session perform. **P1 ✅ merged (`4f072100`); P2 ✅ merged (`f852d2bc`, the risk-concentration phase — single authority kept, wrap-restart tested); P3 (grid commands) next.**
6. MEDIA_BACKEND P1–P3 (Metal era) — decode/encode traits; independent of everything.
   **P1 ⏸ PARKED 2026-07-03: committed §3 trait can't wrap the shipped thread-split +
   zero-alloc reuse-pool decode — needs a §3 addendum before code
   (`docs/DESIGN_HARDENING_QUEUE.md` item 1). No near-wave dependent, so parking is free.**
7. GIG_RESILIENCE P2 (breadcrumb + `--resume`) — needs nothing from other designs.

**Wave 2 — the show becomes playable end-to-end.**
8. PERFORM_SURFACE P1 (chrome-hosted perform substrate, timeline perform migrated).
9. GIG_RESILIENCE P3 (understudy + arming) — immediately after 8, per the hard edge.
10. PERFORM_SURFACE P2 (session perform) — after 5.
11. PROJECTION_MAPPING (corner-pin first — serves the GT2000HDR portrait rig).
12. LED_STRIPS P1 (patch + sACN, no island dep), then P2–P3 after 4.
13. ABLETON_SHOW_SYNC — the score importer; makes gig-resilience rejoin exact and is
    the core Ableton workflow. No hard edge, high instrument value.

**Wave 3 — authoring depth + the AI surface.**
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

## 4. The 3D track (added 2026-07-03)

The hold on MATERIAL_SYSTEM is resolved: the 3D-scene discussion produced
`docs/REALTIME_3D_DESIGN.md`, which consumes the material contract unchanged.
Sequence within the track: MATERIAL M1–M5 → REALTIME_3D P1–P7 →
SIMULATIONS P1–P4 (`docs/SIMULATIONS_DESIGN.md` — XPBD cloth/liquids) →
IMPORT P1–P4 (`docs/IMPORT_DESIGN.md` — glTF scenes, beat-retimed animation,
MDD/PC2 caches = SIMULATIONS lane 1, texture sets). IMPORT P5 (Resolume funnel)
and P6 (TD funnel) hang off the main waves instead (session mode / MCP) — they
ship as v1.x campaigns, before this track. The track is independent of waves 0–3
(renderer-internal) and can run parallel to them once VOCAB apply has landed;
per the version map it is **the paid v2.0 upgrade**, so it starts after v1.0
ships unless Peter re-ranks.

## 5. Maintenance

When a design ships: flip its doc's status line, remove its row here, and re-check
edges that pointed at it. When Peter re-ranks: move rows between waves freely; hard
edges (§2) are the only invariant. This doc is the single place build order lives —
individual design docs state their own prerequisites but never sequence each other.
