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
| NODE_VOCABULARY_AUDIT (apply pass) | none | full |
| GIG_RESILIENCE_DESIGN | none for P1–P2; P3 after PERFORM_SURFACE P1 (see §3) | full |
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

Not in the queue: **LIVE_AUDIO_TRIGGERS** is IN PROGRESS (phases 0–6 done) — finish
in flight, don't re-queue. **COMPETITIVE_STEAL_PASS** is a closed record.

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

Grouped in waves; within a wave, items are independent and order is free.

**Wave 0 — lock the names, protect the work.**
1. VOCAB apply pass (§9 order in its doc).
2. GIG_RESILIENCE P1 (autosave + save-error surfacing + crash.log rotation) — cheap,
   protects everything built after it, zero dependencies.
3. Finish LIVE_AUDIO_TRIGGERS (in flight).

**Wave 1 — the stage foundations.**
4. MULTI_DISPLAY P1–P3 (core model → island rendering → multi-output present) — the
   widest unblock in the corpus: projection mapping, LED P2, and the rig work all
   ride it.
5. SESSION_MODE — second performance surface; unblocks session perform.
6. MEDIA_BACKEND P1–P3 (Metal era) — decode/encode traits; independent of everything.
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

**Background track (long-running, parallel to everything):**
- VULKAN phases 1–4. Start when Peter says; nothing in waves 0–3 waits on it, and
  its two dependents (ML ONNX, MEDIA_BACKEND §6) are explicitly later-tier.

## 4. The 3D track (added 2026-07-03)

The hold on MATERIAL_SYSTEM is resolved: the 3D-scene discussion produced
`docs/REALTIME_3D_DESIGN.md`, which consumes the material contract unchanged.
Sequence within the track: MATERIAL M1–M5 → REALTIME_3D P1–P7 →
SIMULATIONS P1–P4 (`docs/SIMULATIONS_DESIGN.md` — XPBD cloth/liquids; its lane-1
baked playback lives in the future import design). The track is independent of
waves 0–3 (renderer-internal) and can run parallel to them once VOCAB apply has
landed; Peter ranks when it starts. glTF/Blender import (strategic, per Peter)
gets its own design when scheduled and lands on top of REALTIME_3D +
SIMULATIONS lane 1.

## 5. Maintenance

When a design ships: flip its doc's status line, remove its row here, and re-check
edges that pointed at it. When Peter re-ranks: move rows between waves freely; hard
edges (§2) are the only invariant. This doc is the single place build order lives —
individual design docs state their own prerequisites but never sequence each other.
